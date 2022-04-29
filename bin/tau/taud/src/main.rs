use async_std::sync::Arc;
use serde::{Deserialize, Serialize};
use std::fs::create_dir_all;

use async_executor::Executor;
use crypto_box::{aead::Aead, Box, SecretKey, KEY_SIZE};
use easy_parallel::Parallel;
use futures::{select, FutureExt};
use log::{error, info, warn};
use simplelog::{ColorChoice, TermLogger, TerminalMode};
use smol::future;
use structopt_toml::StructOptToml;

use darkfi::{
    async_daemonize,
    raft::Raft,
    rpc::rpcserver::{listen_and_serve, RpcServerConfig},
    util::{
        cli::{log_config, spawn_config},
        expand_path,
        path::get_config_path,
        serial::{deserialize, serialize, SerialDecodable, SerialEncodable},
    },
    Error, Result,
};

mod error;
mod jsonrpc;
mod month_tasks;
mod settings;
mod task_info;
mod util;

use crate::{
    error::TaudResult,
    jsonrpc::JsonRpcInterface,
    month_tasks::MonthTasks,
    settings::{Args, CONFIG_FILE, CONFIG_FILE_CONTENTS},
    task_info::TaskInfo,
    util::{load, save},
};

#[derive(Debug, Clone, SerialEncodable, SerialDecodable, Serialize, Deserialize)]
pub struct MsgPayload {
    nonce: Vec<u8>,
    payload: Vec<u8>,
}

async_daemonize!(realmain);
async fn realmain(settings: Args, executor: Arc<Executor<'_>>) -> Result<()> {
    let datastore_path = expand_path(&settings.datastore)?;

    // mkdir datastore_path if not exists
    create_dir_all(datastore_path.join("month"))?;
    create_dir_all(datastore_path.join("task"))?;

    let mut rng = crypto_box::rand_core::OsRng;

    let secret_key = match load::<[u8; KEY_SIZE]>(&datastore_path.join("secret_key")) {
        Ok(t) => SecretKey::try_from(t)?,
        Err(_) => {
            info!(target: "tau", "generating a new secret key");
            let secret = SecretKey::generate(&mut rng);
            let sk_string = secret.as_bytes();
            save::<[u8; KEY_SIZE]>(&datastore_path.join("secret_key"), sk_string)
                .map_err(Error::from)?;
            secret
        }
    };

    let public_key = secret_key.public_key();
    let msg_box = Box::new(&public_key, &secret_key);

    //
    // RPC
    //
    let server_config = RpcServerConfig {
        socket_addr: settings.rpc_listen,
        use_tls: false,
        // this is all random filler that is meaningless bc tls is disabled
        identity_path: Default::default(),
        identity_pass: Default::default(),
    };

    let (rpc_snd, rpc_rcv) = async_channel::unbounded::<Option<TaskInfo>>();

    let rpc_interface = Arc::new(JsonRpcInterface::new(rpc_snd, datastore_path.clone()));

    let executor_cloned = executor.clone();
    let rpc_listener_taks =
        executor_cloned.spawn(listen_and_serve(server_config, rpc_interface, executor.clone()));

    let net_settings = settings.net;

    //
    //Raft
    //
    let datastore_raft = datastore_path.join("tau.db");
    let mut raft = Raft::<Vec<u8>>::new(net_settings.inbound, datastore_raft)?;

    let raft_sender = raft.get_broadcast();
    let commits = raft.get_commits();
    let initial_sync_raft_sender = raft_sender.clone();

    let datastore_path_cloned = datastore_path.clone();
    let recv_update: smol::Task<TaudResult<()>> = executor.spawn(async move {
        info!(target: "tau", "Start initial sync");
        info!(target: "tau", "Upload local tasks");
        let tasks = MonthTasks::load_current_open_tasks(&datastore_path)?;

        for task in tasks {
            info!(target: "tau", "send local task {:?}", task);

            let nonce = crypto_box::generate_nonce(&mut rng);
            let payload = &serialize(&task)[..];
            let encrypted_payload = msg_box.encrypt(&nonce, payload).unwrap();

            let msg = MsgPayload { nonce: nonce.to_vec(), payload: encrypted_payload };
            let ser_msg = serialize(&msg);

            initial_sync_raft_sender.send(ser_msg).await.map_err(Error::from)?;
        }

        loop {
            select! {
                task = rpc_rcv.recv().fuse() => {
                    let task = task.map_err(Error::from)?;
                    if let Some(tk) = task {
                        info!(target: "tau", "save the received task {:?}", tk);
                        tk.save(&datastore_path_cloned)?;

                        let nonce = crypto_box::generate_nonce(&mut rng);
                        let payload = &serialize(&tk)[..];
                        let encrypted_payload = msg_box.encrypt(&nonce, payload).unwrap();

                        let msg = MsgPayload {
                            nonce: nonce.to_vec(),
                            payload: encrypted_payload,
                        };
                        let ser_msg = serialize(&msg);

                        raft_sender.send(ser_msg).await.map_err(Error::from)?;
                    }
                }
                task = commits.recv().fuse() => {
                    let task = task.map_err(Error::from)?;

                    let recv: MsgPayload = deserialize(&task)?;
                    let nonce = recv.nonce.as_slice();
                    let message = match msg_box.decrypt(nonce.try_into().unwrap(), &recv.payload[..]){
                        Ok(m) => m,
                        Err(_) => {
                            error!("Invalid secret or public key");
                            vec![]
                        },
                    };

                    let task: TaskInfo = deserialize(&message)?;
                    info!(target: "tau", "receive update from the commits {:?}", task);
                    task.save(&datastore_path_cloned)?;
                }

            }
        }
    });

    let (signal, shutdown) = async_channel::bounded::<()>(1);
    ctrlc_async::set_async_handler(async move {
        warn!(target: "tau", "taud start() Exit Signal");
        // cleaning up tasks running in the background
        signal.send(()).await.unwrap();
        rpc_listener_taks.cancel().await;
        recv_update.cancel().await;
    })
    .unwrap();

    // blocking
    raft.start(net_settings.into(), executor.clone(), shutdown.clone()).await?;

    Ok(())
}
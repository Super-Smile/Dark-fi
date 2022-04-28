use std::{
    env::{temp_dir, var},
    fs::{self, File},
    io::{self, Read, Write},
    net::SocketAddr,
    ops::Index,
    process::Command,
};

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime};
use clap::{Parser, Subcommand};
use log::error;
use prettytable::{cell, format, row, Cell, Row, Table};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use darkfi::{Error, Result};

pub const CONFIG_FILE_CONTENTS: &[u8] = include_bytes!("../../taud_config.toml");

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TauConfig {
    /// JSON-RPC listen URL
    pub rpc_listen: SocketAddr,
}

#[derive(Subcommand)]
pub enum CliTauSubCommands {
    /// Add a new task
    Add {
        /// Specify task title
        #[clap(short, long)]
        title: Option<String>,
        /// Specify task description
        #[clap(long)]
        desc: Option<String>,
        /// Assign task to user
        #[clap(short, long)]
        assign: Option<String>,
        /// Task project (can be hierarchical: crypto.zk)
        #[clap(short, long)]
        project: Option<String>,
        /// Due date in DDMM format: "2202" for 22 Feb
        #[clap(short, long)]
        due: Option<String>,
        /// Project rank single precision decimal real value: 4.8761
        #[clap(short, long)]
        rank: Option<f32>,
    },
    /// Update/Edit an existing task by ID
    Update {
        /// Task ID
        id: u64,
        /// Field's name (ex title)
        key: String,
        /// New value
        value: String,
    },
    /// Set task state
    SetState {
        /// Task ID
        id: u64,
        /// Set task state
        state: String,
    },
    /// Get task state
    GetState {
        /// Task ID
        id: u64,
    },
    /// Set comment for a task
    SetComment {
        /// Task ID
        id: u64,
        /// Comment author
        author: String,
        /// Comment content
        content: String,
    },
    /// Get task's comments
    GetComment {
        /// Task ID
        id: u64,
    },
    /// List open tasks
    List {},
    /// Get task by ID
    Get {
        /// Task ID
        id: u64,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskInfo {
    pub ref_id: String,
    pub id: u32,
    pub title: String,
    pub desc: String,
    pub assign: Vec<String>,
    pub project: Vec<String>,
    pub due: Option<i64>,
    pub rank: f32,
    pub created_at: i64,
    pub events: Vec<Value>,
    pub comments: Vec<Value>,
}

/// Tau cli
#[derive(Parser)]
#[clap(name = "tau")]
#[clap(author, version, about)]
pub struct CliTau {
    /// Increase verbosity
    #[clap(short, parse(from_occurrences))]
    pub verbose: u8,
    /// Sets a custom config file
    #[clap(short, long)]
    pub config: Option<String>,
    #[clap(subcommand)]
    pub command: Option<CliTauSubCommands>,
    #[clap(multiple_values = true)]
    /// Search criteria (zero or more)
    pub filter: Vec<String>,
}

pub fn due_as_timestamp(due: &str) -> Option<i64> {
    if due.len() == 4 {
        let (day, month) = (due[..2].parse::<u32>().unwrap(), due[2..].parse::<u32>().unwrap());

        let mut year = Local::today().year();

        if month < Local::today().month() {
            year += 1;
        }

        if month == Local::today().month() && day < Local::today().day() {
            year += 1;
        }

        let dt = NaiveDate::from_ymd(year, month, day).and_hms(12, 0, 0);

        return Some(dt.timestamp())
    }

    if due.len() > 4 {
        error!("due date must be of length 4 (e.g \"1503\" for 15 March)");
    }

    None
}

pub fn set_title() -> Result<String> {
    print!("Title: ");
    io::stdout().flush()?;
    let mut t = String::new();
    io::stdin().read_line(&mut t)?;

    if t.is_empty() {
        error!("You can't have a task without a title");
        return Err(Error::OperationFailed)
    }

    if &t[(t.len() - 1)..] == "\n" {
        t.pop();
    }

    Ok(t)
}

pub fn desc_in_editor() -> Result<Option<String>> {
    // Create a temporary file with some comments inside
    let mut file_path = temp_dir();
    file_path.push("temp_file");
    File::create(&file_path)?;
    fs::write(
        &file_path,
        "\n# Write task description above this line\n# These lines will be removed\n",
    )?;

    // Calling env var {EDITOR} on temp file
    let editor = match var("EDITOR") {
        Ok(t) => t,
        Err(e) => {
            error!("EDITOR {}", e);
            return Err(Error::OperationFailed)
        }
    };
    Command::new(editor).arg(&file_path).status()?;

    // Whatever has been written in temp file, will be read here
    let mut lines = String::new();
    File::open(file_path)?.read_to_string(&mut lines)?;

    // Store only non-comment lines
    let mut description = String::new();
    for line in lines.split('\n') {
        if !line.starts_with('#') {
            description.push_str(line);
            description.push('\n');
        }
    }
    description.pop();

    Ok(Some(description))
}

pub fn get_comments(rep: Value) -> Result<String> {
    let task: Value = serde_json::from_value(rep)?;

    let comments: Vec<Value> = serde_json::from_value(task["comments"].clone())?;
    let mut result = String::new();

    for comment in comments {
        result.push_str(comment["author"].as_str().ok_or(Error::OperationFailed)?);
        result.push_str(": ");
        result.push_str(comment["content"].as_str().ok_or(Error::OperationFailed)?);
        result.push('\n');
    }
    result.pop();

    Ok(result)
}

pub fn get_events(rep: Value) -> Result<String> {
    let task: Value = serde_json::from_value(rep)?;

    let events: Vec<Value> = serde_json::from_value(task["events"].clone())?;
    let mut ev = String::new();

    for event in events {
        ev.push_str("State changed to ");
        ev.push_str(event["action"].as_str().ok_or(Error::OperationFailed)?);
        ev.push_str(" at ");
        ev.push_str(&timestamp_to_date(event["timestamp"].clone(), "datetime"));
        ev.push('\n');
    }
    ev.pop();

    Ok(ev)
}

pub fn timestamp_to_date(timestamp: Value, dt: &str) -> String {
    let result = if timestamp.is_u64() {
        let timestamp = timestamp.as_i64().unwrap();
        match dt {
            "date" => {
                NaiveDateTime::from_timestamp(timestamp, 0).date().format("%A %-d %B").to_string()
            }
            "datetime" => {
                NaiveDateTime::from_timestamp(timestamp, 0).format("%H:%M %A %-d %B").to_string()
            }
            _ => "".to_string(),
        }
    } else {
        "".to_string()
    };

    result
}

pub fn get_from_task(task: Value, value: &str) -> Result<String> {
    let vec_values: Vec<Value> = serde_json::from_value(task[value].clone())?;
    let mut result = String::new();
    for (i, _) in vec_values.iter().enumerate() {
        if !result.is_empty() {
            result.push(',');
        }
        result.push_str(vec_values.index(i).as_str().unwrap());
    }
    Ok(result)
}

fn sort_and_filter(tasks: Vec<Value>, filter: Option<String>) -> Result<Vec<Value>> {
    let filter = match filter {
        Some(f) => f,
        None => "all".to_string(),
    };

    let mut filtered_tasks: Vec<Value> = match filter.as_str() {
        "all" => tasks,

        "open" => tasks
            .into_iter()
            .filter(|task| {
                let events = task["events"].as_array().unwrap().to_owned();

                let state = match events.last() {
                    Some(s) => s["action"].as_str().unwrap(),
                    None => "open",
                };
                state == "open"
            })
            .collect(),

        "pause" => tasks
            .into_iter()
            .filter(|task| {
                let events = task["events"].as_array().unwrap().to_owned();

                let state = match events.last() {
                    Some(s) => s["action"].as_str().unwrap(),
                    None => "open",
                };
                state == "pause"
            })
            .collect(),

        "month" => tasks
            .into_iter()
            .filter(|task| {
                let date = task["created_at"].as_i64().unwrap();
                let task_month = NaiveDateTime::from_timestamp(date, 0).month();
                let this_month = Local::today().month();
                task_month == this_month
            })
            .collect(),

        _ if filter.contains("assign:") | filter.contains("project:") => {
            let kv: Vec<&str> = filter.split(':').collect();
            let key = kv[0];
            let value = kv[1];

            tasks
                .into_iter()
                .filter(|task| {
                    task[key]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|s| s.as_str().unwrap())
                        .any(|x| x == value)
                })
                .collect()
        }

        _ if filter.contains("rank>") | filter.contains("rank<") => {
            let kv: Vec<&str> = if filter.contains('>') {
                filter.split('>').collect()
            } else {
                filter.split('<').collect()
            };
            let key = kv[0];
            let value = kv[1].parse::<f32>()?;

            tasks
                .into_iter()
                .filter(|task| {
                    let rank = task[key].as_f64().unwrap_or(0.0) as f32;
                    if filter.contains('>') {
                        rank > value
                    } else {
                        rank < value
                    }
                })
                .collect()
        }

        _ => tasks,
    };

    filtered_tasks.sort_by(|a, b| b["rank"].as_f64().partial_cmp(&a["rank"].as_f64()).unwrap());

    Ok(filtered_tasks)
}

pub fn list_tasks(rep: Value, filter: Vec<String>) -> Result<()> {
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);
    table.set_titles(row!["ID", "Title", "Project", "Assigned", "Due", "Rank"]);

    let tasks: Vec<Value> = serde_json::from_value(rep)?;

    // we match up to 3 filters to keep things simple and avoid using loops
    let tasks = match filter.len() {
        1 => sort_and_filter(tasks, Some(filter[0].clone()))?,
        2 => {
            let res = sort_and_filter(tasks, Some(filter[0].clone()))?;
            sort_and_filter(res, Some(filter[1].clone()))?
        }
        3 => {
            let res1 = sort_and_filter(tasks, Some(filter[0].clone()))?;
            let res2 = sort_and_filter(res1, Some(filter[1].clone()))?;
            sort_and_filter(res2, Some(filter[2].clone()))?
        }
        _ => sort_and_filter(tasks, None)?,
    };

    let (max_rank, min_rank) = if !tasks.is_empty() {
        (
            serde_json::from_value(tasks[0]["rank"].clone())?,
            serde_json::from_value(tasks[tasks.len() - 1]["rank"].clone())?,
        )
    } else {
        (0.0, 0.0)
    };

    for task in tasks {
        let events: Vec<Value> = serde_json::from_value(task["events"].clone())?;
        let state = match events.last() {
            Some(s) => s["action"].as_str().unwrap(),
            None => "open",
        };

        let rank = task["rank"].as_f64().unwrap_or(0.0) as f32;

        let (max_style, min_style, mid_style, gen_style) = if state == "open" {
            ("bFC", "Fb", "Fc", "")
        } else {
            ("iFYBd", "iFYBd", "iFYBd", "iFYBd")
        };

        table.add_row(Row::new(vec![
            Cell::new(&task["id"].to_string()).style_spec(gen_style),
            Cell::new(task["title"].as_str().unwrap()).style_spec(gen_style),
            Cell::new(&get_from_task(task.clone(), "project")?).style_spec(gen_style),
            Cell::new(&get_from_task(task.clone(), "assign")?).style_spec(gen_style),
            Cell::new(&timestamp_to_date(task["due"].clone(), "date")).style_spec(gen_style),
            if rank == max_rank {
                Cell::new(&rank.to_string()).style_spec(max_style)
            } else if rank == min_rank {
                Cell::new(&rank.to_string()).style_spec(min_style)
            } else {
                Cell::new(&rank.to_string()).style_spec(mid_style)
            },
        ]));
    }
    table.printstd();

    Ok(())
}
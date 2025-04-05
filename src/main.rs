use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow};
use crossbeam_channel::{bounded, select, tick, unbounded};
use crossterm::cursor::SavePosition;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};
use log::{debug, info};
use rayon::prelude::*;
use regex::Regex;
use rusqlite::types::ToSql;
use structopt::StructOpt;

use nginx::{available_variables, format_to_pattern};
use processor::{Processor, generate_processor};

mod nginx;
mod processor;

const STDIN: &str = "STDIN";

// Common field names.
const STATUS_TYPE: &str = "status_type";
const BYTES_SENT: &str = "bytes_sent";
const REQUEST_PATH: &str = "request_path";

#[derive(Debug, StructOpt)]
#[structopt(
    author,
    name = "topngx",
    about = "top for NGINX",
    rename_all = "kebab-case"
)]
struct Options {
    /// The access log to parse.
    #[structopt(short, long)]
    access_log: Option<String>,

    /// The specific log format with which to parse.
    #[structopt(short, long, default_value = "combined")]
    format: String,

    /// Group by this variable.
    #[structopt(short, long, default_value = "request_path")]
    group_by: String,

    /// Having clause.
    #[structopt(short = "w", long, default_value = "1")]
    having: u64,

    /// Refresh the statistics using this interval which is given in seconds.
    #[structopt(short, long, default_value = "2")]
    interval: u64,

    /// Tail the specified log file. You cannot tail standard input.
    #[structopt(short = "t", long)]
    follow: bool,

    /// The number of records to limit for each query.
    #[structopt(short, long, default_value = "10")]
    limit: u64,

    /// Order of output for the default queries.
    #[structopt(short, long, default_value = "count")]
    order_by: String,

    #[structopt(subcommand)]
    subcommand: Option<SubCommand>,
}

// The list of subcommands available to use.
#[derive(Debug, StructOpt)]
enum SubCommand {
    /// Print the average of the given fields.
    Avg(Fields),

    /// List the available fields as well as the access log and format being used.
    Info,

    /// Print out the supplied fields with the given limit.
    Print(Fields),

    /// Supply a custom query.
    Query(Query),

    /// Compute the sum of the given fields.
    Sum(Fields),

    /// Find the top values for the given fields.
    Top(Fields),
}

#[derive(Debug, StructOpt)]
struct Fields {
    /// A space Separated list of field names.
    fields: Vec<String>,
}

#[derive(Debug, StructOpt)]
struct Query {
    /// A space separated list of field names.
    #[structopt(short, long)]
    fields: Vec<String>,

    /// The supplied query. You typically will want to use your shell to quote it.
    #[structopt(short, long)]
    query: String,
}

fn tail(
    opts: &Options,
    access_log: &str,
    fields: Option<Vec<String>>,
    queries: Option<Vec<String>>,
) -> Result<()> {
    const SLEEP: u64 = 100;

    // Save our cursor position.
    execute!(io::stdout(), SavePosition)?;

    let f = File::open(access_log)?;
    let stat = f.metadata()?;
    let mut len = stat.len();
    let mut tail_reader = BufReader::new(f);
    tail_reader.seek(SeekFrom::Start(len))?;

    let pattern = format_to_pattern(&opts.format)?;
    let processor = generate_processor(opts, fields, queries)?;
    let (tx, rx) = unbounded();
    let ticker = tick(Duration::from_secs(opts.interval));

    // The interrupt handling plumbing.
    let (stop_tx, stop_rx) = bounded(0);
    let running = Arc::new(AtomicBool::new(true));
    let handler_r = Arc::clone(&running);

    ctrlc::set_handler(move || {
        handler_r.store(false, Ordering::SeqCst);
    })?;

    let reader_handle = thread::spawn(move || -> Result<()> {
        loop {
            select! {
                recv(stop_rx) -> _ => { return Ok(()); }
                default => {
                    let mut line = String::new();
                    let n_read = tail_reader.read_line(&mut line)?;

                    if n_read > 0 {
                        len += n_read as u64;
                        tail_reader.seek(SeekFrom::Start(len))?;
                        line.pop(); // Remove the newline character.
                        debug!("tail read: {}", line);
                        tx.send(line)?;
                    } else {
                        debug!("tail sleeping for {} milliseconds", SLEEP);
                        thread::sleep(Duration::from_millis(SLEEP));
                    }
                }
            }
        }
    });

    let mut lines = Vec::new();
    while running.load(Ordering::SeqCst) {
        select! {
            recv(rx) -> line => {
                lines.push(line?);
                parse_input(&lines, &pattern, &processor)?;
                lines.clear();
            }
            recv(ticker) -> _ => {
                execute!(io::stdout(), Clear(ClearType::All))?;
                processor.report(opts.follow)?;
            }
        }
    }

    // We got an interrupt, so stop the reading thread.
    stop_tx.send(())?;

    // The join will panic if the thread panics but otherwise it will propagate the return value up
    // to the main thread.
    reader_handle
        .join()
        .expect("the file reading thread should not have panicked")
}

// Either read from STDIN or the file specified.
fn input_source(access_log: &str) -> Result<Box<dyn BufRead>> {
    if access_log == STDIN {
        return Ok(Box::new(BufReader::new(io::stdin())));
    }
    Ok(Box::new(BufReader::new(File::open(access_log)?)))
}

fn run(opts: &Options, fields: Option<Vec<String>>, queries: Option<Vec<String>>) -> Result<()> {
    let access_log = match &opts.access_log {
        Some(l) => l,
        None => {
            if atty::isnt(atty::Stream::Stdin) {
                STDIN
            } else {
                return Err(anyhow!("STDIN is a TTY"));
            }
        }
    };
    info!("access log: {}", access_log);
    info!("access log format: {}", opts.format);

    // We cannot tail STDIN.
    if opts.follow && access_log == STDIN {
        return Err(anyhow!("cannot tail STDIN"));
    }

    // We need to tail the log file.
    if opts.follow {
        return tail(opts, access_log, fields, queries);
    }

    let input = input_source(access_log)?;
    let lines = input
        .lines()
        .filter_map(|l| l.ok())
        .collect::<Vec<String>>();
    let pattern = format_to_pattern(&opts.format)?;
    let processor = generate_processor(opts, fields, queries)?;
    parse_input(&lines, &pattern, &processor)?;
    processor.report(opts.follow)
}

fn parse_input(lines: &[String], pattern: &Regex, processor: &Processor) -> Result<()> {
    let fields = processor.fields.clone();
    let records: Vec<_> = lines
        .par_iter()
        .filter_map(|line| match pattern.captures(line) {
            None => None,
            Some(c) => {
                let mut record: Vec<(String, Box<dyn ToSql + Send + Sync>)> = vec![];

                for field in &fields {
                    if field == STATUS_TYPE {
                        let status = c.name("status").map_or("", |m| m.as_str());
                        let status_type = status.parse::<u16>().unwrap_or(0) / 100;
                        record.push((format!(":{}", field), Box::new(status_type)));
                    } else if field == BYTES_SENT {
                        let bytes_sent = c.name("body_bytes_sent").map_or("", |m| m.as_str());
                        let bytes_sent = bytes_sent.parse::<u32>().unwrap_or(0);
                        record.push((format!(":{}", field), Box::new(bytes_sent)));
                    } else if field == REQUEST_PATH {
                        if c.name("request_uri").is_some() {
                            record.push((
                                format!(":{}", field),
                                Box::new(c.name("request_uri").unwrap().as_str().to_string()),
                            ));
                        } else {
                            let uri = c.name("request").map_or("", |m| m.as_str());
                            record.push((format!(":{}", field), Box::new(uri.to_string())));
                        }
                    } else {
                        let value = c.name(field).map_or("", |m| m.as_str());
                        record.push((format!(":{}", field), Box::new(String::from(value))));
                    }
                }

                Some(record)
            }
        })
        .collect();

    processor.process(records)
}

fn avg_subcommand(opts: &Options, fields: Vec<String>) -> Result<()> {
    let avg_fields: Vec<String> = fields.iter().map(|f| format!("AVG({f})", f = f)).collect();
    let selections = avg_fields.join(", ");
    let query = format!("SELECT {selections} FROM log", selections = selections);
    debug!("average sub command query: {}", query);
    run(opts, Some(fields), Some(vec![query]))
}

fn info_subcommand(opts: &Options) -> Result<()> {
    println!(
        "access log file: {}",
        opts.access_log
            .clone()
            .unwrap_or_else(|| String::from(STDIN))
    );
    println!("access log format: {}", opts.format);
    println!(
        "available variables to query: {}",
        available_variables(&opts.format)?
    );

    Ok(())
}

fn print_subcommand(opts: &Options, fields: Vec<String>) -> Result<()> {
    let selections = fields.join(", ");
    let query = format!(
        "SELECT {selections} FROM log GROUP BY {selections}",
        selections = selections
    );
    debug!("print sub command query: {}", query);
    run(opts, Some(fields), Some(vec![query]))
}

fn query_subcommand(opts: &Options, fields: Vec<String>, query: String) -> Result<()> {
    debug!("custom query: {}", query);
    run(opts, Some(fields), Some(vec![query]))
}

fn sum_subcommand(opts: &Options, fields: Vec<String>) -> Result<()> {
    let sum_fields: Vec<String> = fields.iter().map(|f| format!("SUM({f})", f = f)).collect();
    let selections = sum_fields.join(", ");
    let query = format!("SELECT {selections} FROM log", selections = selections);
    debug!("sum sub command query: {}", query);
    run(opts, Some(fields), Some(vec![query]))
}

fn top_subcommand(opts: &Options, fields: Vec<String>) -> Result<()> {
    let mut queries = Vec::with_capacity(fields.len());

    for f in &fields {
        let query = format!(
            "SELECT {field}, COUNT(1) AS count FROM log \
            GROUP BY {field} ORDER BY COUNT DESC LIMIT {limit}",
            field = f,
            limit = opts.limit
        );
        debug!("top sub command query: {}", query);
        queries.push(query);
    }

    run(opts, Some(fields), Some(queries))
}

fn main() -> Result<()> {
    env_logger::init();

    let opts = Options::from_args();
    debug!("options: {:?}", opts);

    if let Some(sc) = &opts.subcommand {
        match sc {
            SubCommand::Avg(f) => avg_subcommand(&opts, f.fields.clone())?,
            SubCommand::Info => info_subcommand(&opts)?,
            SubCommand::Print(f) => print_subcommand(&opts, f.fields.clone())?,
            SubCommand::Query(q) => query_subcommand(&opts, q.fields.clone(), q.query.clone())?,
            SubCommand::Sum(f) => sum_subcommand(&opts, f.fields.clone())?,
            SubCommand::Top(f) => top_subcommand(&opts, f.fields.clone())?,
        }
        return Ok(());
    }

    run(&opts, None, None)
}

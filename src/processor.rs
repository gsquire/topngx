use std::fmt::Debug;
use std::io::{self, Write};

use anyhow::Result;
use crossterm::cursor::RestorePosition;
use crossterm::execute;
use log::debug;
use rusqlite::types::{ToSql, Value};
use rusqlite::{params, Connection};
use tabwriter::TabWriter;

use super::Options;

/// The main processing engine for all of the statistics.
pub(crate) struct Processor {
    columns: String,
    conn: Connection,
    pub(crate) fields: Vec<String>,
    placeholders: String,
    queries: Vec<String>,
}

impl Processor {
    /// Given the fields to keep track of and the respective queries, return a new Processor.
    fn new(fields: Vec<String>, queries: Vec<String>) -> Result<Processor> {
        Ok(Processor {
            columns: fields.join(", "),
            conn: Connection::open_in_memory()?,
            fields: fields.clone(),
            placeholders: fields
                .iter()
                .map(|f| format!(":{}", f))
                .collect::<Vec<String>>()
                .join(", "),
            queries,
        })
    }

    /// After establishing a new connection, create the table and indexes we need.
    fn initialize(&self) -> Result<()> {
        let create_stmt = format!("CREATE TABLE log ({})", self.columns);
        debug!("create table statement: {}", create_stmt);
        self.conn.execute(&create_stmt, params![])?;

        for (i, field) in self.fields.iter().enumerate() {
            let index_stmt = format!(
                "CREATE INDEX log_idx{i} on log ({field})",
                i = i,
                field = field
            );
            debug!("create index statement: {}", index_stmt);
            self.conn.execute(&index_stmt, params![])?;
        }

        Ok(())
    }

    /// Insert all of the given records into the database.
    pub(crate) fn process(
        &self,
        records: Vec<Vec<(String, Box<dyn ToSql + Send + Sync>)>>,
    ) -> Result<()> {
        let insert_stmt = format!(
            "INSERT INTO LOG ({columns}) VALUES ({placeholders})",
            columns = self.columns,
            placeholders = self.placeholders
        );
        debug!("insert records statement: {}", insert_stmt);

        let mut stmt = self.conn.prepare_cached(&insert_stmt)?;
        for record in records {
            stmt.execute_named(
                &record
                    .iter()
                    .map(|r| (r.0.as_str(), &r.1 as &dyn ToSql))
                    .collect::<Vec<(&str, &dyn ToSql)>>(),
            )?;
        }

        Ok(())
    }

    /// Run the queries as specified by the user.
    pub(crate) fn report(&self) -> Result<()> {
        for query in &self.queries {
            debug!("report query: {}", query);

            let mut stmt = self.conn.prepare_cached(&query)?;
            let rows = stmt.query_map(params![], |r| {
                let columns = r
                    .column_names()
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<String>>();
                let col_count = r.column_count();
                let mut row = Vec::with_capacity(col_count);

                for i in 0..col_count {
                    row.push(r.get_raw_checked(i)?.into());
                }

                Ok(QueryResult { columns, row })
            })?;

            let stdout = io::stdout();
            let mut tw = TabWriter::new(stdout.lock());
            let mut wrote_headers = false;
            for r in rows {
                let r = r?;

                if !wrote_headers {
                    writeln!(&mut tw, "{}", r.columns.join("\t"))?;
                    wrote_headers = true;
                }

                for val in r.row {
                    match val {
                        Value::Null => write!(&mut tw, "null\t")?,
                        Value::Integer(i) => write!(&mut tw, "{}\t", i)?,
                        Value::Real(r) => write!(&mut tw, "{:.2}\t", r)?,
                        Value::Text(t) => write!(&mut tw, "{}\t", t)?,
                        Value::Blob(b) => write!(&mut tw, "{}\t", String::from_utf8(b)?)?,
                    }
                }
                writeln!(&mut tw)?;
            }
            tw.flush()?;
        }

        // Restore our original cursor position.
        execute!(io::stdout(), RestorePosition)?;

        Ok(())
    }
}

/// This represents a generic query result with column names and a row as a result.
#[derive(Debug)]
pub(crate) struct QueryResult {
    columns: Vec<String>,
    row: Vec<Value>,
}

pub(crate) fn generate_processor(
    opts: &Options,
    fields: Option<Vec<String>>,
    queries: Option<Vec<String>>,
) -> Result<Processor> {
    let mut log_fields;
    match fields {
        Some(f) => log_fields = f,
        None => {
            log_fields = vec![
                String::from(super::STATUS_TYPE),
                String::from(super::BYTES_SENT),
            ];
            if !log_fields.contains(&opts.group_by) {
                log_fields.push(opts.group_by.clone());
            }
        }
    }

    let default_summary_query = format!(
        "SELECT count(1) AS count,
AVG(bytes_sent) as avg_bytes_sent,
COUNT(CASE WHEN status_type = 2 THEN 1 END) AS '2XX',
COUNT(CASE WHEN status_type = 3 THEN 1 END) AS '3XX',
COUNT(CASE WHEN status_type = 4 THEN 1 END) AS '4XX',
COUNT(CASE WHEN status_type = 5 THEN 1 END) AS '5XX'
FROM log
ORDER BY {order_by} DESC
LIMIT {limit};",
        order_by = opts.order_by,
        limit = opts.limit
    );

    let default_detailed_query = format!(
        "SELECT {group_by},
COUNT(1) AS count,
AVG(bytes_sent) AS avg_bytes_sent,
COUNT(CASE WHEN status_type = 2 THEN 1 END) AS '2XX',
COUNT(CASE WHEN status_type = 3 THEN 1 END) AS '3XX',
COUNT(CASE WHEN status_type = 4 THEN 1 END) AS '4XX',
COUNT(CASE WHEN status_type = 5 THEN 1 END) AS '5XX'
FROM log
GROUP BY {group_by}
HAVING {having_opt}
ORDER BY {order_by} DESC
LIMIT {limit};",
        group_by = opts.group_by,
        having_opt = opts.having,
        order_by = opts.order_by,
        limit = opts.limit
    );

    let log_queries = match queries {
        Some(q) => q,
        None => vec![default_summary_query, default_detailed_query],
    };

    let p = Processor::new(log_fields, log_queries)?;
    p.initialize()?;

    Ok(p)
}

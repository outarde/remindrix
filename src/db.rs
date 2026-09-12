use std::{
    path::PathBuf,
    sync::Arc, 
};
use tokio_rusqlite::Connection;
use anyhow::{Result, Context};

struct Migration {
    version: i32,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/000_init.sql"),
    },
];

/// Database initialization.
pub async fn init_db(data_dir: &PathBuf) -> Result<Connection> {
    let path = data_dir.join("remindrix.db");
    let conn = Connection::open(&path).await
        .with_context(|| format!("Failed to open database at {:?}", path))?;

    run_migrations(&conn).await?;

    Ok(conn)
}

/// DB Migration.
async fn run_migrations(conn: &Connection) -> Result<()> {
    // Get current version.
    let current_version: i32 = conn.call(|c| {
        c.query_row("PRAGMA user_version", [], |row| row.get(0))
    }).await.unwrap_or(0);

    if current_version as usize >= MIGRATIONS.len() {
        tracing::info!("Database is up to date (version {})", current_version);
        return Ok(());
    }

    tracing::info!("Starting migrations: current version {}, target version {}", 
        current_version, MIGRATIONS.len());

    // Queries.
    conn.call(move |c| -> Result<(), tokio_rusqlite::Error> {
        let tx = c.transaction()?;
        
        for migration in MIGRATIONS.iter().filter(|m| m.version > current_version) {
            tracing::info!("Applying migration version {}", migration.version);
            
            tx.execute(migration.sql, [])?;

            // Update PRAGMA user_version.
            let pragma_query = format!("PRAGMA user_version = {}", migration.version);
            tx.execute(&pragma_query, [])?;
        }

        tx.commit()?;
        Ok(())

        // Ok::<_, tokio_rusqlite::Error>(())
        // We can use OK(()) without turbo-fish if we specify
        // -> Result<(), tokio_rusqlite::Error> in function result.
    }).await.map_err(|e| anyhow::anyhow!("Migration failed: {}", e))?;

    tracing::info!("Database migration completed successfully");
    Ok(())
}

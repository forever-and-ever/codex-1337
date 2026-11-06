fn main() {
    // sqlx embeds existing files, but Cargo must also notice added migrations.
    for directory in [
        "migrations",
        "goals_migrations",
        "memory_migrations",
        "queue_migrations",
        "thread_history_migrations",
    ] {
        println!("cargo:rerun-if-changed={directory}");
    }
}

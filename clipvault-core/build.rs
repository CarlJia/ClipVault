fn main() {
    // KTD3: Enable trigram tokenizer for FTS5.
    // This requires building libsqlite3-sys from source with the trigram flag.
    // NOTE: rusqlite 0.32 bundled SQLite does NOT include trigram by default.
    // If this build fails or trigram is still unavailable at runtime, set the
    // SQLITE3_SYS_FEATURE 环境变量 or use system SQLite instead.
    println!("cargo:rustc-env=SQLITE_FTS5_TOKENIZER_TRIGRAMS=1");
}

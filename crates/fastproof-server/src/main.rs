use clap::Parser;

fn main() {
    fastproof_blob::install_crypto_provider();
    let cli = fastproof_server::cli::Cli::parse();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: starting the runtime failed: {e}");
            std::process::exit(1);
        }
    };
    let code = match rt.block_on(fastproof_server::cli::run(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

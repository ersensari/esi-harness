#![recursion_limit = "256"]

#[cfg(not(any(feature = "rustls-tls", feature = "native-tls")))]
compile_error!("At least one of `rustls-tls` or `native-tls` features must be enabled");

#[cfg(all(feature = "rustls-tls", feature = "native-tls"))]
compile_error!("Features `rustls-tls` and `native-tls` are mutually exclusive");

pub mod cli;
pub mod commands;
pub mod logging;
pub mod recipes;
pub mod scenario_tests;
pub mod session;
pub mod signal;

use anyhow::Result;

// Re-export commonly used types
pub use cli::Cli;
pub use session::CliSession;

pub fn run() -> Result<()> {
    #[cfg(windows)]
    {
        let _ = console::Term::stdout().features().colors_supported();
        let _ = console::Term::stderr().features().colors_supported();
    }

    let handle = std::thread::Builder::new()
        .name("esi-studio-cli-main".to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed to build Tokio runtime");
            runtime.block_on(async {
                if let Err(error) = logging::setup_logging(None) {
                    eprintln!("Warning: Failed to initialize logging: {error}");
                }

                let result = cli::cli().await;

                #[cfg(feature = "otel")]
                if goose::otel::otlp::is_otlp_initialized() {
                    goose::otel::otlp::shutdown_otlp();
                }

                result
            })
        })
        .map_err(|error| anyhow::anyhow!("Failed to spawn ESI-Studio CLI thread: {error}"))?;

    handle
        .join()
        .map_err(|_| anyhow::anyhow!("ESI-Studio CLI thread panicked"))?
}

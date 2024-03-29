use clap::Parser;
use env_logger::Builder;
use log::LevelFilter;

#[derive(Debug, Clone, Parser)]
pub struct VerbosityArgs {
    #[clap(long, help = "Log raw request/response traffic of providers")]
    log_traffic: bool,
}

impl VerbosityArgs {
    pub fn setup_logging(&self) {
        let mut builder = Builder::new();

        if self.log_traffic {
            builder.filter_module("starknet_providers", LevelFilter::Trace);
        }

        builder.init();
    }
}

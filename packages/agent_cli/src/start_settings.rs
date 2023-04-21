use clap::Parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct CliArgs {
    #[clap(short, long)]
    stdout_logs: bool,

    #[clap(short, long)]
    config_file: Option<String>,

    #[clap(short, long)]
    use_linux_path_defaults: bool,
}

#[derive(Debug)]
pub struct StartSettings {
    pub config_file_path: String,
    pub try_ui: bool,
}

impl StartSettings {
    pub fn parse() -> StartSettings {
        let args: CliArgs = CliArgs::parse();

        if args.use_linux_path_defaults {
            #[cfg(not(target_family = "unix"))]
            {
                println!("--use-linux-path-defaults is only supported on UNIX systems");
                std::process::exit(1);
            }
        }

        #[cfg(target_os = "windows")]
        {
            use crate::tray::setup_tray;
            let _task = tokio::spawn(setup_tray());
        }

        StartSettings {
            config_file_path: args.config_file.unwrap_or_else(||
                if args.use_linux_path_defaults {
                    "/etc/playit/playit.toml".to_string()
                } else {
                    "./playit.toml".to_string()
                }
            ),
            try_ui: !args.stdout_logs,
        }
    }
}
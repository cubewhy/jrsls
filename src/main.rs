use clap::{Parser, ValueEnum};
use jrsls::backend::LspBackend;
use tower_lsp::{LspService, Server};

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum Mode {
    #[value(alias = "tcp")]
    TcpSocket,
    Stdin,
}

#[derive(Debug, Parser)]
#[command(name = "jrsls", about = "A lightweight Java LSP server")]
struct Cli {
    /// LSP transport: stdin/stdout or TCP socket
    #[arg(long, value_enum, default_value_t = Mode::TcpSocket)]
    mode: Mode,

    /// TCP port to listen on (when mode=tcp-socket)
    #[arg(long, default_value_t = 9257)]
    port: u16,

    /// Override JAVA_HOME (defaults to the JAVA_HOME environment variable)
    #[arg(long)]
    java_home: Option<String>,

    /// Java language level (affects keyword set); examples: 8, 11, 17, 21
    #[arg(long, default_value_t = 17)]
    java_version: u16,

    /// Comma-separated keyword list override
    #[arg(long)]
    java_keywords: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let cli = Cli::parse();
    setup_java_home(&cli);
    let keywords = collect_keywords(&cli);

    let config = jrsls::backend::ServerConfig { keywords };
    let (service, socket) = LspService::new(move |client| LspBackend::new(client, config.clone()));

    match cli.mode {
        Mode::TcpSocket => {
            let addr = format!("127.0.0.1:{}", cli.port);
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            tracing::info!("Starting jrsls in tcp-socket mode at {}", addr);

            let (stream, _) = listener.accept().await?;
            let (read, write) = tokio::io::split(stream);
            Server::new(read, write, socket).serve(service).await;
        }
        Mode::Stdin => {
            tracing::info!("Starting jrsls in stdin/stdout mode");
            let stdin = tokio::io::stdin();
            let stdout = tokio::io::stdout();
            Server::new(stdin, stdout, socket).serve(service).await;
        }
    }

    Ok(())
}

fn setup_java_home(cli: &Cli) {
    let java_home = cli
        .java_home
        .clone()
        .or_else(|| std::env::var("JAVA_HOME").ok());

    if let Some(path) = java_home {
        unsafe {
            // SAFETY: Setting an environment variable is acceptable here to propagate
            // the configured JAVA_HOME to the rest of the process.
            std::env::set_var("JAVA_HOME", &path);
        }
        tracing::info!("Using JAVA_HOME={}", path);
    } else {
        tracing::warn!("JAVA_HOME is not set; Java resolution may be limited");
    }
}

fn collect_keywords(cli: &Cli) -> Vec<String> {
    if let Some(list) = &cli.java_keywords {
        return list
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    let mut keywords = vec![
        "abstract",
        "assert",
        "boolean",
        "break",
        "byte",
        "case",
        "catch",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extends",
        "final",
        "finally",
        "float",
        "for",
        "goto",
        "if",
        "implements",
        "import",
        "instanceof",
        "int",
        "interface",
        "long",
        "native",
        "new",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "static",
        "strictfp",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "transient",
        "try",
        "void",
        "volatile",
        "while",
    ];

    if cli.java_version >= 10 {
        keywords.push("var");
    }
    if cli.java_version >= 14 {
        keywords.push("yield");
    }
    if cli.java_version >= 16 {
        keywords.extend(["record", "sealed", "non-sealed", "permits"]);
    }

    keywords.into_iter().map(|s| s.to_string()).collect()
}

use clap::Parser;

#[derive(Parser)]
#[command(name = "git-lfs", version, about = "Git LFS — large file storage for git")]
struct Cli {}

fn main() {
    Cli::parse();
    println!("git-lfs {}", env!("CARGO_PKG_VERSION"));
}

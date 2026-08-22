use cbz_tools_optimizer_launcher::EmbeddedAsset;

mod assets {
    use super::EmbeddedAsset;
    include!(concat!(env!("OUT_DIR"), "/embedded_cli.rs"));
}

fn main() {
    std::process::exit(cbz_tools_optimizer_launcher::run_cli(assets::ASSETS));
}

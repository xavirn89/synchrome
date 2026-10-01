fn main() {
    for b in synchrome_browser::discover_browsers() {
        println!(
            "{} ({}) access={:?} version={:?} profiles={}",
            b.display_name,
            b.user_data_dir,
            b.access,
            b.version,
            b.profiles.len()
        );
        for p in &b.profiles {
            println!(
                "    - {} [{}] {:?}",
                p.display_name.clone().unwrap_or_default(),
                p.dir_name,
                p.access
            );
        }
    }
}

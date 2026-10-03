fn main() {
    // the publishable client values may be baked in at build time (release bundles)
    println!("cargo:rerun-if-env-changed=SUPABASE_URL");
    println!("cargo:rerun-if-env-changed=SUPABASE_ANON_KEY");
}

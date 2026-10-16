//! Prints the OpenAPI document: cargo run --locked -p tikkit-api --bin openapi > apps/api/openapi.json
fn main() {
    println!(
        "{}",
        tikkit_api::openapi()
            .to_pretty_json()
            .expect("serializable OpenAPI")
    );
}

use std::collections::HashMap;

fn main() {
    let mut map: HashMap<String, usize> = HashMap::new();
    let name = String::from("test");

    // PERF: Avoid unconditional allocation on cache hit
    let count = if let Some(count) = map.get_mut(&name) {
        *count += 1;
        *count
    } else {
        map.insert(name.clone(), 1);
        1
    };
    println!("{}", count);
}

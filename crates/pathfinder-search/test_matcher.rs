use grep_matcher::Matcher;
use grep_regex::RegexMatcher;

fn main() {
    let m = RegexMatcher::new("hello").unwrap();
    let res = m.find(b"say hello there").unwrap();
    println!("{:?}", res);
}

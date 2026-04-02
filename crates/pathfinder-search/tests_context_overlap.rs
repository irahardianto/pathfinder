use grep_regex::RegexMatcherBuilder;
use grep_searcher::{SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch};

struct TestSink;
impl Sink for TestSink {
    type Error = std::io::Error;
    fn matched(&mut self, _s: &grep_searcher::Searcher, m: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        println!("MATCH: {}", String::from_utf8_lossy(m.bytes()).trim());
        Ok(true)
    }
    fn context(&mut self, _s: &grep_searcher::Searcher, c: &SinkContext<'_>) -> Result<bool, Self::Error> {
        println!("{:?} CONTEXT: {}", c.kind(), String::from_utf8_lossy(c.bytes()).trim());
        Ok(true)
    }
}

fn main() {
    let matcher = RegexMatcherBuilder::new().build("token").unwrap();
    let mut searcher = SearcherBuilder::new().before_context(1).after_context(1).build();
    let text = b"1\ntoken\ntoken\n4";
    searcher.search_slice(&matcher, text, TestSink).unwrap();
}

#![allow(clippy::similar_names)]
// ── language_to_file_glob tests ─────────────────────────────────────────

#[test]
fn test_language_to_file_glob_rust() {
    assert_eq!(super::language_to_file_glob("rust"), "**/*.rs");
}

#[test]
fn test_language_to_file_glob_typescript() {
    assert_eq!(super::language_to_file_glob("typescript"), "**/*.{ts,tsx}");
}

#[test]
fn test_language_to_file_glob_javascript() {
    assert_eq!(super::language_to_file_glob("javascript"), "**/*.{js,jsx}");
}

#[test]
fn test_language_to_file_glob_python() {
    assert_eq!(super::language_to_file_glob("python"), "**/*.py");
}

#[test]
fn test_language_to_file_glob_go() {
    assert_eq!(super::language_to_file_glob("go"), "**/*.go");
}

#[test]
fn test_language_to_file_glob_vue() {
    assert_eq!(
        super::language_to_file_glob("vue"),
        "**/*.{vue,ts,tsx,js,jsx,mjs,cjs}"
    );
}

#[test]
fn test_language_to_file_glob_java() {
    assert_eq!(super::language_to_file_glob("java"), "**/*.java");
}

#[test]
fn test_language_to_file_glob_unknown_defaults_to_catch_all() {
    assert_eq!(super::language_to_file_glob("haskell"), "**/*");
    assert_eq!(super::language_to_file_glob(""), "**/*");
}

// ── definition_patterns tests ──────────────────────────────────────────

#[test]
fn test_definition_patterns_rust_fn() {
    let patterns = super::definition_patterns("rs", "my_function");
    assert!(!patterns.is_empty(), "must return at least one pattern");
    // First pattern should match function definitions
    let re = regex::Regex::new(&patterns[0]).expect("valid regex");
    assert!(
        re.is_match("pub async fn my_function("),
        "must match 'pub async fn my_function('"
    );
    assert!(
        re.is_match("fn my_function("),
        "must match bare 'fn my_function('"
    );
    assert!(
        !re.is_match("let my_function = 42"),
        "must not match variable assignment"
    );
}

#[test]
fn test_definition_patterns_rust_struct() {
    let patterns = super::definition_patterns("rs", "MyStruct");
    assert!(patterns.len() >= 2, "must return patterns for types too");
    let re = regex::Regex::new(&patterns[1]).expect("valid regex");
    assert!(
        re.is_match("pub(crate) struct MyStruct {"),
        "must match 'pub(crate) struct MyStruct {{'"
    );
    assert!(
        re.is_match("enum MyStruct {"),
        "must match 'enum MyStruct {{'"
    );
}

#[test]
fn test_definition_patterns_ts_export_class() {
    let patterns = super::definition_patterns("ts", "AuthService");
    assert!(!patterns.is_empty());
    // Second pattern matches classes/interfaces
    let re = regex::Regex::new(&patterns[1]).expect("valid regex");
    assert!(
        re.is_match("export default class AuthService {"),
        "must match 'export default class AuthService {{'"
    );
    assert!(
        re.is_match("export interface AuthService {"),
        "must match 'export interface AuthService {{'"
    );
}

// ── Vue definition_patterns tests (DELIVERABLE C) ─────────────────────

#[test]
fn test_definition_patterns_vue_function() {
    let patterns = super::definition_patterns("vue", "handleClick");
    assert!(!patterns.is_empty(), "vue must have definition patterns");
    let re = regex::Regex::new(&patterns[0]).expect("valid regex");
    assert!(
        re.is_match("export async function handleClick("),
        "must match 'export async function handleClick('"
    );
    assert!(
        re.is_match("function handleClick("),
        "must match bare 'function handleClick('"
    );
}

#[test]
fn test_definition_patterns_vue_const_assignment() {
    let patterns = super::definition_patterns("vue", "handleClick");
    assert!(patterns.len() >= 3);
    let re = regex::Regex::new(&patterns[2]).expect("valid regex");
    assert!(
        re.is_match("const handleClick = () => {}"),
        "must match 'const handleClick = () => {{}}'"
    );
    assert!(
        re.is_match("export const handleClick = () => {}"),
        "must match 'export const handleClick = () => {{}}'"
    );
    assert!(
        re.is_match("let handleClick: Handler = () => {}"),
        "must match typed assignment 'let handleClick: Handler ='"
    );
}

#[test]
fn test_definition_patterns_vue_ref() {
    let patterns = super::definition_patterns("vue", "count");
    assert!(patterns.len() >= 3);
    let re = regex::Regex::new(&patterns[2]).expect("valid regex");
    assert!(
        re.is_match("const count = ref(0)"),
        "must match 'const count = ref(0)'"
    );
    assert!(
        re.is_match("const count = reactive({ value: 0 })"),
        "must match 'const count = reactive(...)'"
    );
    assert!(
        re.is_match("const count = computed(() => 0)"),
        "must match 'const count = computed(...)'"
    );
}

#[test]
fn test_definition_patterns_vue_define_macros() {
    let patterns_props = super::definition_patterns("vue", "props");
    let patterns_emit = super::definition_patterns("vue", "emit");
    assert!(patterns_props.len() >= 5);
    let re_props = regex::Regex::new(&patterns_props[4]).expect("valid regex");
    let re_emit = regex::Regex::new(&patterns_emit[4]).expect("valid regex");
    assert!(
        re_props.is_match("const props = defineProps<{ id: string }>()"),
        "must match 'const props = defineProps(...)'"
    );
    assert!(
        re_emit.is_match("const emit = defineEmits<{ (e: 'save'): void }>()"),
        "must match 'const emit = defineEmits(...)'"
    );
    assert!(
        re_props.is_match("const props = withDefaults(defineProps<{ }>(), {})"),
        "must match 'const props = withDefaults(...)'"
    );
}

#[test]
fn test_definition_patterns_py_async_def() {
    let patterns = super::definition_patterns("py", "process_order");
    assert!(!patterns.is_empty());
    let re = regex::Regex::new(&patterns[0]).expect("valid regex");
    assert!(
        re.is_match("async def process_order("),
        "must match 'async def process_order('"
    );
    assert!(
        re.is_match("def process_order("),
        "must match 'def process_order('"
    );
}

#[test]
fn test_definition_patterns_py_class() {
    let patterns = super::definition_patterns("py", "MyClass");
    assert!(patterns.len() >= 2);
    let re = regex::Regex::new(&patterns[1]).expect("valid regex");
    assert!(re.is_match("class MyClass:"), "must match 'class MyClass:'");
}

#[test]
fn test_definition_patterns_go_receiver_method() {
    let patterns = super::definition_patterns("go", "HandleRequest");
    assert!(!patterns.is_empty());
    let re = regex::Regex::new(&patterns[0]).expect("valid regex");
    assert!(
        re.is_match("func (s *Service) HandleRequest("),
        "must match receiver method"
    );
    assert!(
        re.is_match("func HandleRequest("),
        "must match bare function"
    );
}

#[test]
fn test_definition_patterns_go_type() {
    let patterns = super::definition_patterns("go", "UserService");
    assert!(patterns.len() >= 3, "go must have func + type + const/var");
    let re = regex::Regex::new(&patterns[2]).expect("valid regex");
    assert!(
        re.is_match("type UserService struct {"),
        "must match 'type UserService struct {{'"
    );
}

#[test]
fn test_definition_patterns_unknown_extension_uses_fallback() {
    let patterns = super::definition_patterns("java", "MyClass");
    assert!(!patterns.is_empty());
    // Java has its own patterns
    let re = regex::Regex::new(&patterns[0]).expect("valid regex");
    assert!(
        re.is_match("public class MyClass {"),
        "must match Java class declaration"
    );
}

#[test]
fn test_definition_patterns_catch_all_extension() {
    let patterns = super::definition_patterns("unknown_ext", "foo");
    assert_eq!(
        patterns.len(),
        1,
        "catch-all must return exactly one pattern"
    );
    let re = regex::Regex::new(&patterns[0]).expect("valid regex");
    assert!(re.is_match("foo"), "must match bare word");
    assert!(!re.is_match("foobar"), "must not match partial word");
}

#[test]
fn test_definition_patterns_special_chars_escaped() {
    // Symbol name with regex special characters must be escaped
    let patterns = super::definition_patterns("rs", "my+function");
    assert!(!patterns.is_empty());
    let re = regex::Regex::new(&patterns[0]).expect("valid regex");
    // Must match literal "my+function", not "myXfunction"
    assert!(re.is_match("fn my+function("));
    assert!(!re.is_match("fn myXfunction("));
}

#[test]
fn test_definition_patterns_all_languages_compile() {
    // Verify every extension returns valid regex patterns
    let extensions = [
        "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "vue", "xyz",
    ];
    for ext in &extensions {
        let patterns = super::definition_patterns(ext, "TestSymbol");
        for (i, pattern) in patterns.iter().enumerate() {
            assert!(
                regex::Regex::new(pattern).is_ok(),
                "pattern {i} for ext '{ext}' must be valid regex: {pattern}"
            );
        }
    }
}

// ── Java definition_patterns tests (DELIVERABLE E) ───────────────────

#[test]
fn test_definition_patterns_java_class() {
    let patterns = super::definition_patterns("java", "MyClass");
    assert!(!patterns.is_empty(), "java must have definition patterns");
    let re = regex::Regex::new(&patterns[0]).expect("valid regex");
    assert!(
        re.is_match("public class MyClass {"),
        "must match 'public class MyClass {{'"
    );
    assert!(
        re.is_match("private static final class MyClass {"),
        "must match 'private static final class MyClass {{'"
    );
}

#[test]
fn test_definition_patterns_java_constructor() {
    let patterns = super::definition_patterns("java", "MyClass");
    assert!(!patterns.is_empty(), "java must have definition patterns");
    // Look for a pattern that matches constructors
    let constructor_pattern = patterns.iter().find(|p| p.contains("MyClass\\s*\\("));
    assert!(
        constructor_pattern.is_some(),
        "java must have a constructor pattern"
    );
    let re = regex::Regex::new(constructor_pattern.unwrap()).expect("valid regex");
    assert!(
        re.is_match("public MyClass(String name) {"),
        "must match 'public MyClass(String name) {{'"
    );
    assert!(
        re.is_match("MyClass(String name, int age) {"),
        "must match bare 'MyClass(String name, int age) {{'"
    );
    assert!(
        re.is_match("private MyClass() {"),
        "must match 'private MyClass() {{'"
    );
}

#[test]
fn test_definition_patterns_java_record() {
    let patterns = super::definition_patterns("java", "Person");
    assert!(!patterns.is_empty(), "java must have definition patterns");
    // Look for a pattern that matches records
    let record_pattern = patterns.iter().find(|p| p.contains("record"));
    assert!(record_pattern.is_some(), "java must have a record pattern");
    let re = regex::Regex::new(record_pattern.unwrap()).expect("valid regex");
    assert!(
        re.is_match("public record Person(String name) {"),
        "must match 'public record Person(String name) {{'"
    );
    assert!(
        re.is_match("record Person(String name, int age) {"),
        "must match bare 'record Person(String name, int age) {{'"
    );
    assert!(
        re.is_match("private final record Person(String name) {"),
        "must match 'private final record Person(String name) {{'"
    );
}

#[test]
fn test_definition_patterns_java_sealed_class() {
    let patterns = super::definition_patterns("java", "Shape");
    assert!(!patterns.is_empty(), "java must have definition patterns");
    // Look for a pattern that matches sealed classes
    let sealed_pattern = patterns.iter().find(|p| p.contains("sealed"));
    assert!(
        sealed_pattern.is_some(),
        "java must have a sealed class/interface pattern"
    );
    let re = regex::Regex::new(sealed_pattern.unwrap()).expect("valid regex");
    assert!(
        re.is_match("public sealed class Shape permits Circle, Square {"),
        "must match 'public sealed class Shape permits Circle, Square {{'"
    );
    assert!(
        re.is_match("sealed interface Shape permits Circle {"),
        "must match 'sealed interface Shape permits Circle {{'"
    );
    assert!(
        re.is_match("private sealed abstract class Shape {"),
        "must match 'private sealed abstract class Shape {{'"
    );
}

#[test]
fn test_definition_patterns_java_annotated_method() {
    let patterns = super::definition_patterns("java", "myService");
    // The last pattern is for methods with annotations
    let method_pattern = patterns.last().expect("java should have patterns");
    let re = regex::Regex::new(method_pattern).expect("valid regex");
    assert!(
        re.is_match("@Bean public MyService myService()"),
        "must match '@Bean public MyService myService()'"
    );
    assert!(
        re.is_match("@Override public void myService()"),
        "must match '@Override public void myService()'"
    );
    assert!(
        re.is_match("@GetMapping public Response myService()"),
        "must match '@GetMapping public Response myService()'"
    );
}

#[test]
fn test_definition_patterns_java_primitive_return() {
    let patterns = super::definition_patterns("java", "process");
    assert!(!patterns.is_empty(), "java must have definition patterns");
    // The last pattern matches methods with any return type
    let method_pattern = patterns.last().expect("java should have patterns");
    let re = regex::Regex::new(method_pattern).expect("valid regex");
    assert!(re.is_match("void process()"), "must match 'void process()'");
    assert!(
        re.is_match("public boolean process()"),
        "must match 'public boolean process()'"
    );
    assert!(
        re.is_match("private int process()"),
        "must match 'private int process()'"
    );
    assert!(
        re.is_match("protected String process()"),
        "must match 'protected String process()'"
    );
    assert!(
        re.is_match("static final double process()"),
        "must match 'static final double process()'"
    );
}

#[test]
fn test_definition_patterns_java_generic_return() {
    let patterns = super::definition_patterns("java", "process");
    assert!(!patterns.is_empty(), "java must have definition patterns");
    // The last pattern matches methods with generic return types
    let method_pattern = patterns.last().expect("java should have patterns");
    let re = regex::Regex::new(method_pattern).expect("valid regex");
    assert!(
        re.is_match("public List<String> process()"),
        "must match 'public List<String> process()'"
    );
    assert!(
        re.is_match("Map<String, Integer> process()"),
        "must match 'Map<String, Integer> process()'"
    );
}

#[test]
fn test_definition_patterns_java_array_return() {
    let patterns = super::definition_patterns("java", "process");
    assert!(!patterns.is_empty(), "java must have definition patterns");
    let method_pattern = patterns.last().expect("java should have patterns");
    let re = regex::Regex::new(method_pattern).expect("valid regex");
    assert!(
        re.is_match("public String[] process()"),
        "must match 'public String[] process()'"
    );
    assert!(
        re.is_match("int[] process()"),
        "must match 'int[] process()'"
    );
    assert!(
        re.is_match("public int[][] process()"),
        "must match 'public int[][] process()' — multi-dimensional array"
    );
    assert!(
        re.is_match("String[][][] process()"),
        "must match 'String[][][] process()' — 3D array"
    );
}

#[test]
fn test_definition_patterns_java_method_with_type_params() {
    let patterns = super::definition_patterns("java", "process");
    assert!(!patterns.is_empty(), "java must have definition patterns");
    // The last pattern matches methods with type parameters
    let method_pattern = patterns.last().expect("java should have patterns");
    let re = regex::Regex::new(method_pattern).expect("valid regex");
    assert!(
        re.is_match("public <T> T process()"),
        "must match 'public <T> T process()'"
    );
    assert!(
        re.is_match("<T, U> Map<T, U> process()"),
        "must match '<T, U> Map<T, U> process()'"
    );
}

// ── Java negative test cases (Deliverable E fixes) ─────────────────────

#[test]
fn test_definition_patterns_java_constructor_rejects_return_types() {
    // CRITICAL-2: Pattern 1 (constructor) must not match methods with return types
    let patterns = super::definition_patterns("java", "MyClass");
    let constructor_pattern = patterns
        .get(1)
        .expect("java should have constructor pattern");
    let re = regex::Regex::new(constructor_pattern).expect("valid regex");
    assert!(
        !re.is_match("public void MyClass()"),
        "must NOT match 'public void MyClass()' - this is a method, not a constructor"
    );
    assert!(
        !re.is_match("private String MyClass()"),
        "must NOT match 'private String MyClass()' - this is a method, not a constructor"
    );
    assert!(
        !re.is_match("protected int MyClass()"),
        "must NOT match 'protected int MyClass()' - this is a method, not a constructor"
    );
}

#[test]
fn test_definition_patterns_java_method_pattern_rejects_new_and_throw() {
    // CRITICAL-1: Pattern 4 must NOT match new ClassName() or throw new MyError()
    let patterns = super::definition_patterns("java", "MyError");
    let method_pattern = patterns.last().expect("java should have method pattern");
    let re = regex::Regex::new(method_pattern).expect("valid regex");
    assert!(
        !re.is_match("throw new MyError(msg)"),
        "must NOT match 'throw new MyError(msg)' - false positive"
    );
    assert!(
        !re.is_match("return new MyError()"),
        "must NOT match 'return new MyError()' - false positive"
    );
    assert!(
        !re.is_match("new MyError().getMessage()"),
        "must NOT match 'new MyError().getMessage()' - false positive"
    );
}

#[test]
fn test_definition_patterns_java_constructor_rejects_new_keyword() {
    let patterns = super::definition_patterns("java", "MyClass");
    let constructor_pattern = patterns
        .get(1)
        .expect("java should have constructor pattern");
    let re = regex::Regex::new(constructor_pattern).expect("valid regex");
    assert!(
        !re.is_match("new MyClass()"),
        "must NOT match 'new MyClass()' - this is a call, not a definition"
    );
    assert!(
        !re.is_match("return new MyClass()"),
        "must NOT match 'return new MyClass()' - this is a call, not a definition"
    );
}

#[test]
fn test_definition_patterns_java_generic_constructor() {
    // MEDIUM-4: Support generic constructors like public <E> MyClass(E item)
    let patterns = super::definition_patterns("java", "MyClass");
    let constructor_pattern = patterns
        .get(1)
        .expect("java should have constructor pattern");
    let re = regex::Regex::new(constructor_pattern).expect("valid regex");
    assert!(
        re.is_match("public <E> MyClass(E item)"),
        "must match 'public <E> MyClass(E item)'"
    );
    assert!(
        re.is_match("<T, U> MyClass(T a, U b)"),
        "must match '<T, U> MyClass(T a, U b)'"
    );
}

#[test]
fn test_definition_patterns_java_nested_generics() {
    // MEDIUM-2: Support nested generics like Map<String, List<Integer>>
    let patterns = super::definition_patterns("java", "process");
    let method_pattern = patterns.last().expect("java should have method pattern");
    let re = regex::Regex::new(method_pattern).expect("valid regex");
    assert!(
        re.is_match("public Map<String, List<Integer>> process()"),
        "must match 'public Map<String, List<Integer>> process()'"
    );
    assert!(
        re.is_match("Map<String, Map<String, Integer>> process()"),
        "must match 'Map<String, Map<String, Integer>> process()'"
    );
}

#[test]
fn test_definition_patterns_java_sealed_no_trailing_whitespace() {
    // MAJOR-2: Pattern should match sealed class at end-of-line (no trailing whitespace)
    let patterns = super::definition_patterns("java", "Shape");
    let class_pattern = patterns.first().expect("java should have class pattern");
    let re = regex::Regex::new(class_pattern).expect("valid regex");
    assert!(
        re.is_match("public sealed class Shape"),
        "must match 'public sealed class Shape' at end-of-line"
    );
    assert!(
        re.is_match("sealed class Shape{"),
        "must match 'sealed class Shape{{' without space before brace"
    );
}

#[test]
fn test_definition_patterns_java_strictfp_method() {
    let patterns = super::definition_patterns("java", "calculate");
    let method_pattern = patterns.last().expect("java should have method pattern");
    let re = regex::Regex::new(method_pattern).expect("valid regex");
    assert!(
        re.is_match("public strictfp void calculate()"),
        "must match 'public strictfp void calculate()'"
    );
    assert!(
        re.is_match("strictfp double calculate(int x)"),
        "must match 'strictfp double calculate(int x)'"
    );
}

#[test]
fn test_definition_patterns_java_strictfp_class() {
    let patterns = super::definition_patterns("java", "MathUtils");
    let class_pattern = patterns.first().expect("java should have class pattern");
    let re = regex::Regex::new(class_pattern).expect("valid regex");
    assert!(
        re.is_match("strictfp class MathUtils"),
        "must match 'strictfp class MathUtils'"
    );
    assert!(
        re.is_match("public strictfp class MathUtils"),
        "must match 'public strictfp class MathUtils'"
    );
}

#[test]
fn test_definition_patterns_java_non_sealed_class() {
    let patterns = super::definition_patterns("java", "Shape");
    let class_pattern = patterns.first().expect("java should have class pattern");
    let re = regex::Regex::new(class_pattern).expect("valid regex");
    assert!(
        re.is_match("non-sealed class Shape"),
        "must match 'non-sealed class Shape'"
    );
    assert!(
        re.is_match("public non-sealed class Shape"),
        "must match 'public non-sealed class Shape'"
    );
}

#[test]
fn test_definition_patterns_java_multi_dimensional_array_return() {
    let patterns = super::definition_patterns("java", "getData");
    let method_pattern = patterns.last().expect("java should have method pattern");
    let re = regex::Regex::new(method_pattern).expect("valid regex");
    assert!(
        re.is_match("public int[][] getData()"),
        "must match 'public int[][] getData()' — 2D array"
    );
    assert!(
        re.is_match("String[][][] getData()"),
        "must match 'String[][][] getData()' — 3D array"
    );
    assert!(
        re.is_match("Map<String, Integer>[][] getData()"),
        "must match 'Map<String, Integer>[][] getData()' — generic 2D array"
    );
}

#[test]
fn test_definition_patterns_java_bounded_generics() {
    let patterns = super::definition_patterns("java", "sort");
    let method_pattern = patterns.last().expect("java should have method pattern");
    let re = regex::Regex::new(method_pattern).expect("valid regex");
    assert!(
        re.is_match("public <T extends Comparable<T>> void sort(List<T> list)"),
        "must match 'public <T extends Comparable<T>> void sort(List<T> list)' — bounded generics"
    );
    let patterns_get = super::definition_patterns("java", "get");
    let method_pattern_get = patterns_get
        .last()
        .expect("java should have method pattern");
    let re_get = regex::Regex::new(method_pattern_get).expect("valid regex");
    assert!(
        re_get.is_match("<K, V extends Serializable> V get(K key)"),
        "must match '<K, V extends Serializable> V get(K key)' — multiple bounded params"
    );
    let patterns2 = super::definition_patterns("java", "MyClass");
    let constructor_pattern = patterns2
        .get(1)
        .expect("java should have constructor pattern");
    let re2 = regex::Regex::new(constructor_pattern).expect("valid regex");
    assert!(
            re2.is_match("public <T extends Comparable<T>> MyClass(T item)"),
            "must match 'public <T extends Comparable<T>> MyClass(T item)' — generic constructor with bounds"
        );
}

#[test]
fn test_definition_patterns_java_static_record() {
    let patterns = super::definition_patterns("java", "Inner");
    let record_pattern = patterns.get(2).expect("java should have record pattern");
    let re = regex::Regex::new(record_pattern).expect("valid regex");
    assert!(
        re.is_match("static record Inner(String name, int value)"),
        "must match 'static record Inner(String name, int value)' — nested static record"
    );
    assert!(
        re.is_match("public static final record Inner(String name)"),
        "must match 'public static final record Inner(String name)' — full modifiers"
    );
}

// ── extract_call_candidates tests ──────────────────────────────────────

#[test]
fn test_extract_call_candidates_rust_basic() {
    let code = r"
            fn process() {
                fetch_user(id);
                validate_order(&order);
                charge_payment(amount);
            }
        ";
    let candidates = super::extract_call_candidates(code, "rust");
    assert!(candidates.contains(&"fetch_user".to_string()));
    assert!(candidates.contains(&"validate_order".to_string()));
    assert!(candidates.contains(&"charge_payment".to_string()));
}

#[test]
fn test_extract_call_candidates_filters_keywords() {
    let code = r"
            fn process() {
                if condition { return; }
                for item in items { do_something(item); }
                while running { check(); }
                match value { _ => {} }
            }
        ";
    let candidates = super::extract_call_candidates(code, "rust");
    assert!(
        !candidates.contains(&"if".to_string()),
        "must filter 'if' keyword"
    );
    assert!(
        !candidates.contains(&"for".to_string()),
        "must filter 'for' keyword"
    );
    assert!(
        !candidates.contains(&"while".to_string()),
        "must filter 'while' keyword"
    );
    assert!(
        !candidates.contains(&"match".to_string()),
        "must filter 'match' keyword"
    );
    assert!(
        !candidates.contains(&"return".to_string()),
        "must filter 'return' keyword"
    );
    assert!(
        candidates.contains(&"do_something".to_string()),
        "must keep real function call"
    );
    assert!(
        candidates.contains(&"check".to_string()),
        "must keep real function call"
    );
}

#[test]
fn test_extract_call_candidates_go_keywords() {
    let code = r"
            func process() {
                if err != nil { return err }
                for _, v := range items { handle(v) }
                select { case <-ch: }
            }
        ";
    let candidates = super::extract_call_candidates(code, "go");
    assert!(!candidates.contains(&"if".to_string()));
    assert!(!candidates.contains(&"for".to_string()));
    assert!(!candidates.contains(&"range".to_string()));
    assert!(!candidates.contains(&"select".to_string()));
    assert!(candidates.contains(&"handle".to_string()));
}

#[test]
fn test_extract_call_candidates_python_keywords() {
    let code = r#"
def process():
    if condition:
        return result
    for item in items:
        handle(item)
    raise ValueError("bad")
        "#;
    let candidates = super::extract_call_candidates(code, "python");
    assert!(!candidates.contains(&"if".to_string()));
    assert!(!candidates.contains(&"for".to_string()));
    assert!(!candidates.contains(&"return".to_string()));
    assert!(!candidates.contains(&"raise".to_string()));
    assert!(candidates.contains(&"handle".to_string()));
}

#[test]
fn test_extract_call_candidates_deduplicates() {
    let code = r"
            fn process() {
                fetch(id);
                fetch(id);
                fetch(id);
            }
        ";
    let candidates = super::extract_call_candidates(code, "rust");
    let count = candidates.iter().filter(|c| *c == "fetch").count();
    assert_eq!(count, 1, "must deduplicate call candidates");
}

#[test]
#[allow(clippy::format_push_string)]
fn test_extract_call_candidates_caps_at_20() {
    // Generate 25 unique function calls
    let mut code = String::from("fn process() {\n");
    for i in 0..25 {
        code.push_str(&format!("    func_{i}(x);\n"));
    }
    code.push('}');

    let candidates = super::extract_call_candidates(&code, "rust");
    assert!(
        candidates.len() <= 20,
        "must cap at 20 candidates, got {}",
        candidates.len()
    );
}

#[test]
fn test_extract_call_candidates_typescript_method_calls() {
    let code = r"
            function process() {
                user.getName();
                order.calculateTotal();
                service.validate(data);
            }
        ";
    let candidates = super::extract_call_candidates(code, "typescript");
    // Method calls (obj.method()) should also be extracted for TS/JS
    assert!(candidates.contains(&"getName".to_string()));
    assert!(candidates.contains(&"calculateTotal".to_string()));
    assert!(candidates.contains(&"validate".to_string()));
}

// ── Vue extract_call_candidates test (DELIVERABLE C) ──────────────────

#[test]
fn test_extract_call_candidates_vue_method_calls() {
    // Vue <script setup> uses same patterns as TypeScript
    let code = r"
            const handleSubmit = () => {
                userService.login(credentials);
                router.push('/dashboard');
                toast.showSuccess();
            }
        ";
    let candidates = super::extract_call_candidates(code, "vue");
    // Method calls (obj.method()) should also be extracted for Vue
    assert!(
        candidates.contains(&"login".to_string()),
        "expected 'login' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"push".to_string()),
        "expected 'push' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"showSuccess".to_string()),
        "expected 'showSuccess' in {candidates:?}"
    );
}

#[test]
fn test_extract_call_candidates_empty_input() {
    let candidates = super::extract_call_candidates("", "rust");
    assert!(candidates.is_empty(), "empty input must return empty vec");
}

#[test]
fn test_extract_call_candidates_no_calls() {
    let code = "let x = 42;\nlet y = x + 1;";
    let candidates = super::extract_call_candidates(code, "rust");
    assert!(
        candidates.is_empty(),
        "no function calls must return empty vec"
    );
}

// ── keywords_for_language tests ────────────────────────────────────────

#[test]
fn test_keywords_for_language_rust() {
    let kw = super::keywords_for_language("rust");
    assert!(kw.contains(&"fn"), "must contain 'fn'");
    assert!(kw.contains(&"struct"), "must contain 'struct'");
    assert!(kw.contains(&"impl"), "must contain 'impl'");
    assert!(kw.contains(&"async"), "must contain 'async'");
    assert!(kw.contains(&"await"), "must contain 'await'");
    assert!(kw.len() > 20, "rust keywords must be comprehensive");
}

#[test]
fn test_keywords_for_language_go() {
    let kw = super::keywords_for_language("go");
    assert!(kw.contains(&"func"), "must contain 'func'");
    assert!(kw.contains(&"defer"), "must contain 'defer'");
    assert!(kw.contains(&"select"), "must contain 'select'");
    assert!(kw.contains(&"chan"), "must contain 'chan'");
}

#[test]
fn test_keywords_for_language_typescript() {
    let kw = super::keywords_for_language("typescript");
    assert!(kw.contains(&"function"), "must contain 'function'");
    assert!(kw.contains(&"class"), "must contain 'class'");
    assert!(kw.contains(&"const"), "must contain 'const'");
}

#[test]
fn test_keywords_for_language_python() {
    let kw = super::keywords_for_language("python");
    assert!(kw.contains(&"def"), "must contain 'def'");
    assert!(kw.contains(&"class"), "must contain 'class'");
    assert!(kw.contains(&"raise"), "must contain 'raise'");
}

#[test]
fn test_keywords_for_language_java() {
    let kw = super::keywords_for_language("java");
    assert!(kw.contains(&"class"), "must contain 'class'");
    assert!(kw.contains(&"interface"), "must contain 'interface'");
    assert!(kw.contains(&"extends"), "must contain 'extends'");
}

// ── Vue keywords_for_language test (DELIVERABLE C) ────────────────────

#[test]
fn test_keywords_for_language_vue() {
    let kw = super::keywords_for_language("vue");
    // TS/JS base keywords
    assert!(kw.contains(&"function"), "must contain 'function'");
    assert!(kw.contains(&"const"), "must contain 'const'");
    // Vue-specific composables
    assert!(kw.contains(&"ref"), "must contain 'ref'");
    assert!(kw.contains(&"reactive"), "must contain 'reactive'");
    assert!(kw.contains(&"computed"), "must contain 'computed'");
    assert!(kw.contains(&"watch"), "must contain 'watch'");
    assert!(kw.contains(&"onMounted"), "must contain 'onMounted'");
    // Vue compiler macros
    assert!(kw.contains(&"defineProps"), "must contain 'defineProps'");
    assert!(kw.contains(&"defineEmits"), "must contain 'defineEmits'");
}

#[test]
fn test_keywords_for_language_unknown_uses_default() {
    let kw = super::keywords_for_language("haskell");
    assert!(kw.contains(&"if"), "default must contain 'if'");
    assert!(kw.contains(&"for"), "default must contain 'for'");
    assert!(kw.contains(&"while"), "default must contain 'while'");
    assert!(kw.contains(&"return"), "default must contain 'return'");
}

#[test]
fn test_try_separator_correction_converts_double_colon_to_dot() {
    assert_eq!(
        super::PathfinderServer::try_separator_correction("cache.rs::AstCache::get_or_parse"),
        Some("cache.rs::AstCache.get_or_parse".to_string())
    );
    assert_eq!(
        super::PathfinderServer::try_separator_correction("file.rs::Struct::method::inner"),
        Some("file.rs::Struct.method.inner".to_string())
    );
    assert_eq!(
        super::PathfinderServer::try_separator_correction("file.rs::simple_symbol"),
        None
    );
    assert_eq!(
        super::PathfinderServer::try_separator_correction("file.rs"),
        None
    );
}

#[test]
fn test_is_workspace_file_heuristics() {
    // Unix absolute paths are not workspace files
    assert!(!super::is_workspace_file("/usr/bin/src/main.rs"));

    // Windows absolute paths are not workspace files
    assert!(!super::is_workspace_file("C:\\projects\\main.rs"));
    assert!(!super::is_workspace_file("D:/projects/main.rs"));
    assert!(!super::is_workspace_file("\\network\\main.rs"));

    // Dependency directories are not workspace files
    assert!(!super::is_workspace_file("node_modules/lodash/index.js"));
    assert!(!super::is_workspace_file("node_modules\\lodash\\index.js"));
    assert!(!super::is_workspace_file(
        "vendor/github.com/pkg/errors/errors.go"
    ));
    assert!(!super::is_workspace_file(
        "vendor\\github.com\\pkg\\errors\\errors.go"
    ));

    // Rust stdlib paths are not workspace files
    assert!(!super::is_workspace_file("std/src/lib.rs"));
    assert!(!super::is_workspace_file("core/src/lib.rs"));
    assert!(!super::is_workspace_file("alloc/src/lib.rs"));
    assert!(!super::is_workspace_file("std"));
    assert!(!super::is_workspace_file("core"));
    assert!(!super::is_workspace_file("alloc"));
    assert!(!super::is_workspace_file("library/std/src/path.rs"));
    assert!(!super::is_workspace_file("library/core/src/lib.rs"));
    assert!(!super::is_workspace_file("library/alloc/src/lib.rs"));
    assert!(!super::is_workspace_file("library\\std\\src\\path.rs"));
    assert!(!super::is_workspace_file("library\\core\\src\\lib.rs"));
    assert!(!super::is_workspace_file("library\\alloc\\src\\lib.rs"));

    // Regular relative source files are workspace files
    assert!(super::is_workspace_file("src/main.rs"));
    assert!(super::is_workspace_file("lib/utils.ts"));

    // Non-source code files are not workspace files
    assert!(!super::is_workspace_file("README.md"));
    assert!(!super::is_workspace_file("package.json"));
}

#[tokio::test]
async fn test_enrich_did_you_mean_all_cases() {
    use super::test_helpers::{make_server_with_lawyer, make_temp_workspace};
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_treesitter::mock::MockSurgeon;

    let mock_surgeon = std::sync::Arc::new(MockSurgeon::new());
    let mock_lawyer = std::sync::Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, _temp_dir) = make_server_with_lawyer(mock_surgeon.clone(), mock_lawyer.clone());

    // Case 1: Separator confusion correction: corrected path not already in suggestions
    let original_suggestions = vec!["src/auth.rs::AuthService".to_string()];
    let enriched = server
        .enrich_did_you_mean("src/auth.rs::AuthService::login", original_suggestions)
        .await;
    assert_eq!(enriched.len(), 2);
    assert_eq!(enriched[0], "src/auth.rs::AuthService.login");
    assert_eq!(enriched[1], "src/auth.rs::AuthService");

    // Case 2: Separator confusion correction: corrected path IS already in suggestions (should not duplicate)
    let original_suggestions = vec![
        "src/auth.rs::AuthService.login".to_string(),
        "src/auth.rs::AuthService".to_string(),
    ];
    let enriched = server
        .enrich_did_you_mean("src/auth.rs::AuthService::login", original_suggestions)
        .await;
    assert_eq!(enriched.len(), 2);
    assert_eq!(enriched[0], "src/auth.rs::AuthService.login");
    assert_eq!(enriched[1], "src/auth.rs::AuthService");

    // Case 3: Empty suggestions -> calls cross-file search find_symbol_impl which succeeds
    let mock_scout = std::sync::Arc::new(pathfinder_search::MockScout::default());
    mock_scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/auth.rs".to_owned(),
            line: 1,
            column: 1,
            content: "fn login() {}".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            version_hash: "hash".to_owned(),
            is_definition: None,
            known: None,
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 1,
        files_in_scope: 1,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    // Enclosing symbol calls: we need to push Ok(None) to mock_surgeon enclosing_symbol_detail_results.
    // find_symbol_impl uses enclosing_symbol_detail() for treesitter-based kind classification.
    // Let's push 100 times to be safe since find_symbol_impl will run parallel searches.
    for _ in 0..100 {
        mock_surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));
    }

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server_with_scout = super::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        mock_scout,
        mock_surgeon.clone(),
        mock_lawyer.clone(),
    );

    let enriched = server_with_scout
        .enrich_did_you_mean("src/auth.rs::login", vec![])
        .await;
    assert!(enriched.contains(&"src/auth.rs::login".to_string()));

    // Case 4: Empty suggestions -> calls cross-file search find_symbol_impl which returns error (path separator in symbol name)
    let enriched_err = server_with_scout
        .enrich_did_you_mean("src/auth.rs::login/error", vec![])
        .await;
    assert!(enriched_err.is_empty());
}

#[tokio::test]
async fn test_read_symbol_scope_enriched_all_cases() {
    use super::test_helpers::{make_scope, make_server_with_lawyer};
    use pathfinder_treesitter::mock::MockSurgeon;

    let mock_surgeon = std::sync::Arc::new(MockSurgeon::new());
    let mock_lawyer = std::sync::Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, _temp_dir) = make_server_with_lawyer(mock_surgeon.clone(), mock_lawyer.clone());

    // Case 1: Surgeon returns Ok(scope) -> read_symbol_scope_enriched returns Ok(scope)
    let scope = make_scope();
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(scope.clone()));
    let semantic_path =
        pathfinder_common::types::SemanticPath::parse("src/auth.rs::login").unwrap();
    let res = server
        .read_symbol_scope_enriched(&semantic_path, "src/auth.rs::login")
        .await;
    assert!(res.is_ok());
    assert_eq!(res.unwrap().content, scope.content);

    // Case 2: Surgeon returns SymbolNotFound error with original suggestions,
    // and semantic path has NO double colons in the symbol chain.
    // It should enrich did_you_mean and return Err(SymbolNotFound).
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Err(pathfinder_treesitter::SurgeonError::SymbolNotFound {
            path: "src/auth.rs::login".to_owned(),
            did_you_mean: vec![],
        }));
    let res = server
        .read_symbol_scope_enriched(&semantic_path, "src/auth.rs::login")
        .await;
    assert!(res.is_err());
    let err = res.unwrap_err();
    let data = err.data.as_ref().expect("error should contain JSON data");
    assert_eq!(data["error"], "SYMBOL_NOT_FOUND");

    // Case 3: Surgeon returns SymbolNotFound error, and semantic path HAS double colons in symbol chain.
    // First try fails with SymbolNotFound.
    // Auto-retry corrects the path (:: -> .) and calls surgeon again, which succeeds.
    let corrected_scope = make_scope();
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Err(pathfinder_treesitter::SurgeonError::SymbolNotFound {
            path: "src/auth.rs::AuthService::login".to_owned(),
            did_you_mean: vec![],
        }));
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(corrected_scope.clone()));

    let semantic_path_with_confusion =
        pathfinder_common::types::SemanticPath::parse("src/auth.rs::AuthService::login").unwrap();
    let res = server
        .read_symbol_scope_enriched(
            &semantic_path_with_confusion,
            "src/auth.rs::AuthService::login",
        )
        .await;
    assert!(res.is_ok());
    assert_eq!(res.unwrap().content, corrected_scope.content);
}

// ── is_source_file: valid source extensions ────────────────────────────

#[test]
fn test_is_source_file_standard_extensions() {
    // All entries in SOURCE_FILE_EXTENSIONS must return true
    let valid = [
        "src/main.rs",
        "cmd/api/main.go",
        "src/index.ts",
        "src/App.tsx",
        "lib/utils.js",
        "components/Button.jsx",
        "lib/esm.mjs",
        "lib/cjs.cjs",
        "app/main.py",
        "stubs/types.pyi",
        "components/App.vue",
        "com/example/Main.java",
    ];
    for path in &valid {
        assert!(
            super::is_source_file(path),
            "expected is_source_file=true for '{path}'"
        );
    }
}

#[test]
fn test_is_source_file_non_source_extensions() {
    // Config, doc, asset files must return false
    let invalid = [
        "README.md",
        "package.json",
        "config.yaml",
        "config.toml",
        "Makefile",
        "Dockerfile",
        ".gitignore",
        "styles.css",
        "index.html",
    ];
    for path in &invalid {
        assert!(
            !super::is_source_file(path),
            "expected is_source_file=false for '{path}'"
        );
    }
}

#[test]
fn test_is_source_file_binary_extensions() {
    // Binary/media files must return false
    let binaries = [
        "image.png",
        "photo.jpg",
        "photo.jpeg",
        "icon.gif",
        "app.exe",
        "library.dll",
        "library.so",
        "archive.zip",
        "archive.tar.gz",
        "font.woff2",
        "data.pdf",
    ];
    for path in &binaries {
        assert!(
            !super::is_source_file(path),
            "expected is_source_file=false for binary '{path}'"
        );
    }
}

#[test]
fn test_is_source_file_no_extension() {
    // Files without extensions (e.g. Makefile, LICENSE) return false
    assert!(!super::is_source_file("Makefile"));
    assert!(!super::is_source_file("LICENSE"));
    assert!(!super::is_source_file("src/"));
}

#[test]
fn test_is_source_file_unsupported_web_extensions() {
    // These are web-adjacent but not in SOURCE_FILE_EXTENSIONS
    let unsupported = [
        "styles.scss",
        "styles.less",
        "schema.graphql",
        "api.proto",
        "App.svelte",
    ];
    for path in &unsupported {
        assert!(
            !super::is_source_file(path),
            "expected is_source_file=false for unsupported web ext '{path}'"
        );
    }
}

// ── definition_patterns: non-empty for all supported languages ─────────

#[test]
fn test_definition_patterns_non_empty_for_all_supported_extensions() {
    let extensions = ["rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "vue"];
    for ext in &extensions {
        let patterns = super::definition_patterns(ext, "SomeSymbol");
        assert!(
            !patterns.is_empty(),
            "definition_patterns must return non-empty for ext '{ext}'"
        );
        assert!(
            patterns.len() >= 2,
            "definition_patterns for '{ext}' should have at least 2 patterns, got {}",
            patterns.len()
        );
    }
}

#[test]
fn test_definition_patterns_tsx_jsx_share_ts_patterns() {
    // tsx/jsx should produce the same pattern set as ts/js
    let ts_patterns = super::definition_patterns("ts", "Foo");
    let tsx_patterns = super::definition_patterns("tsx", "Foo");
    let js_patterns = super::definition_patterns("js", "Foo");
    let jsx_patterns = super::definition_patterns("jsx", "Foo");
    assert_eq!(ts_patterns.len(), tsx_patterns.len());
    assert_eq!(js_patterns.len(), jsx_patterns.len());
    assert_eq!(ts_patterns.len(), js_patterns.len());
}

#[test]
fn test_definition_patterns_go_generic_type() {
    // Go generic type definitions: type Foo[T any] struct {}
    let patterns = super::definition_patterns("go", "Cache");
    assert!(patterns.len() >= 4, "go must have generic type pattern");
    let re = regex::Regex::new(&patterns[3]).expect("valid regex");
    assert!(
        re.is_match("type Cache[K comparable, V any] struct {"),
        "must match generic type definition"
    );
}

#[test]
fn test_definition_patterns_rust_macro_rules() {
    let patterns = super::definition_patterns("rs", "my_macro");
    assert!(patterns.len() >= 4, "rust must have macro_rules pattern");
    let re = regex::Regex::new(&patterns[3]).expect("valid regex");
    assert!(
        re.is_match("macro_rules! my_macro {"),
        "must match 'macro_rules! my_macro {{'"
    );
}

#[test]
fn test_definition_patterns_rust_const_static() {
    let patterns = super::definition_patterns("rs", "MAX_SIZE");
    assert!(patterns.len() >= 3);
    let re = regex::Regex::new(&patterns[2]).expect("valid regex");
    assert!(re.is_match("pub const MAX_SIZE: usize = 100;"));
    assert!(re.is_match("static MAX_SIZE: i32 = 42;"));
}

#[test]
fn test_definition_patterns_python_module_assignment() {
    let patterns = super::definition_patterns("py", "DEFAULT_TIMEOUT");
    assert!(patterns.len() >= 3, "python must have assignment pattern");
    let re = regex::Regex::new(&patterns[2]).expect("valid regex");
    assert!(
        re.is_match("DEFAULT_TIMEOUT = 30"),
        "must match module-level assignment"
    );
    assert!(
        re.is_match("DEFAULT_TIMEOUT: int = 30"),
        "must match typed assignment"
    );
}

#[test]
fn test_definition_patterns_ts_arrow_function() {
    let patterns = super::definition_patterns("ts", "fetchData");
    assert!(patterns.len() >= 4, "ts must have arrow fn pattern");
    let re = regex::Regex::new(&patterns[3]).expect("valid regex");
    assert!(
        re.is_match("export const fetchData = async (url: string): Promise<void> => {"),
        "must match exported async arrow function"
    );
}

#[test]
fn test_definition_patterns_go_const_var() {
    let patterns = super::definition_patterns("go", "MaxRetries");
    assert!(patterns.len() >= 5, "go must have const/var pattern");
    let re = regex::Regex::new(&patterns[4]).expect("valid regex");
    assert!(re.is_match("const MaxRetries = 3"));
    assert!(re.is_match("var MaxRetries int"));
}

// ── extract_call_candidates: additional language coverage ──────────────

#[test]
fn test_extract_call_candidates_python_function_calls() {
    let code = r"
def handle_request(request):
    user = get_current_user(request)
    data = parse_body(request.body)
    result = process_order(data, user)
    send_notification(user.email)
    return format_response(result)
";
    let candidates = super::extract_call_candidates(code, "python");
    assert!(
        candidates.contains(&"get_current_user".to_string()),
        "expected 'get_current_user' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"parse_body".to_string()),
        "expected 'parse_body' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"process_order".to_string()),
        "expected 'process_order' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"send_notification".to_string()),
        "expected 'send_notification' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"format_response".to_string()),
        "expected 'format_response' in {candidates:?}"
    );
    // Keywords filtered
    assert!(!candidates.contains(&"def".to_string()));
    assert!(!candidates.contains(&"return".to_string()));
}

#[test]
fn test_extract_call_candidates_rust_method_calls() {
    let code = r"
fn process(service: &Service) {
    let conn = service.connect();
    let data = conn.fetch_data(id);
    self.validate(data);
    result.unwrap();
}
";
    let candidates = super::extract_call_candidates(code, "rust");
    assert!(
        candidates.contains(&"connect".to_string()),
        "expected 'connect' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"fetch_data".to_string()),
        "expected 'fetch_data' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"validate".to_string()),
        "expected 'validate' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"unwrap".to_string()),
        "expected 'unwrap' in {candidates:?}"
    );
}

#[test]
fn test_extract_call_candidates_go_method_calls() {
    let code = r"
func (s *Server) HandleRequest(w http.ResponseWriter, r *http.Request) {
    user := s.GetUser(r.Context())
    data := s.parseBody(r)
    result := s.processOrder(data, user)
    w.WriteHeader(200)
}
";
    let candidates = super::extract_call_candidates(code, "go");
    assert!(
        candidates.contains(&"GetUser".to_string()),
        "expected 'GetUser' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"parseBody".to_string()),
        "expected 'parseBody' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"processOrder".to_string()),
        "expected 'processOrder' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"WriteHeader".to_string()),
        "expected 'WriteHeader' in {candidates:?}"
    );
    // Go keywords filtered
    assert!(!candidates.contains(&"func".to_string()));
}

#[test]
fn test_extract_call_candidates_java_method_calls() {
    let code = r"
public void processOrder(Order order) {
    User user = userService.findById(order.getUserId());
    validator.validate(order);
    PaymentResult result = paymentGateway.charge(order.getTotal());
    notificationService.send(user.getEmail());
}
";
    let candidates = super::extract_call_candidates(code, "java");
    assert!(
        candidates.contains(&"findById".to_string()),
        "expected 'findById' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"validate".to_string()),
        "expected 'validate' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"charge".to_string()),
        "expected 'charge' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"send".to_string()),
        "expected 'send' in {candidates:?}"
    );
    // Java keywords filtered
    assert!(!candidates.contains(&"new".to_string()));
}

#[test]
fn test_extract_call_candidates_javascript_mixed_calls() {
    let code = r"
function handleSubmit(event) {
    event.preventDefault();
    const data = parseFormData(event.target);
    const result = apiClient.post('/submit', data);
    showNotification('Success');
}
";
    let candidates = super::extract_call_candidates(code, "javascript");
    assert!(
        candidates.contains(&"preventDefault".to_string()),
        "expected 'preventDefault' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"parseFormData".to_string()),
        "expected 'parseFormData' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"post".to_string()),
        "expected 'post' in {candidates:?}"
    );
    assert!(
        candidates.contains(&"showNotification".to_string()),
        "expected 'showNotification' in {candidates:?}"
    );
    // JS keywords filtered
    assert!(!candidates.contains(&"function".to_string()));
    assert!(!candidates.contains(&"const".to_string()));
}

#[test]
fn test_extract_call_candidates_unknown_language_uses_default_keywords() {
    let code = r"
fn process() {
    if condition { return; }
    doWork(x);
}
";
    let candidates = super::extract_call_candidates(code, "unknown_lang");
    // Default keywords (if, return, for, while, etc.) should be filtered
    assert!(!candidates.contains(&"if".to_string()));
    assert!(!candidates.contains(&"return".to_string()));
    assert!(
        candidates.contains(&"doWork".to_string()),
        "real fn call must be kept for unknown language"
    );
}

// ── keywords_for_language: javascript alias ────────────────────────────

#[test]
fn test_keywords_for_language_javascript_matches_typescript() {
    let ts_kw = super::keywords_for_language("typescript");
    let js_kw = super::keywords_for_language("javascript");
    assert_eq!(
        ts_kw.len(),
        js_kw.len(),
        "typescript and javascript must share keyword list"
    );
    for kw in ts_kw {
        assert!(
            js_kw.contains(kw),
            "javascript keywords must contain '{kw}'"
        );
    }
}

// ── candidate_definition_pattern tests ─────────────────────────────────

#[test]
fn test_candidate_definition_pattern_rust() {
    let pat = super::candidate_definition_pattern("rust", "process_order");
    // Bug was: outer (?:...) group was never closed — now fixed.
    let re = regex::Regex::new(&pat).expect("valid regex: outer group must be closed");
    assert!(re.is_match("fn process_order("), "bare fn must match");
    assert!(re.is_match("pub fn process_order("), "pub fn must match");
    assert!(
        re.is_match("pub async fn process_order("),
        "pub async fn must match"
    );
    assert!(
        re.is_match("pub(crate) fn process_order("),
        "pub(crate) fn must match"
    );
    assert!(
        !re.is_match("let process_order ="),
        "variable binding must NOT match"
    );
    assert!(
        !re.is_match("process_order("),
        "bare call site must NOT match"
    );
}

#[test]
fn test_candidate_definition_pattern_go() {
    let pat = super::candidate_definition_pattern("go", "HandleRequest");
    let re = regex::Regex::new(&pat).expect("valid regex");
    assert!(re.is_match("func HandleRequest("));
    assert!(!re.is_match("HandleRequest("));
}

#[test]
fn test_candidate_definition_pattern_typescript() {
    let pat = super::candidate_definition_pattern("typescript", "fetchData");
    let re = regex::Regex::new(&pat).expect("valid regex");
    assert!(re.is_match("function fetchData("));
    assert!(re.is_match("export function fetchData("));
    assert!(re.is_match("export const fetchData ="));
    assert!(re.is_match("const fetchData ="));
}

#[test]
fn test_candidate_definition_pattern_python() {
    let pat = super::candidate_definition_pattern("python", "process");
    let re = regex::Regex::new(&pat).expect("valid regex");
    assert!(re.is_match("def process("));
    assert!(re.is_match("async def process("));
}

#[test]
fn test_candidate_definition_pattern_java_delegates_to_java_resolve() {
    let pat = super::candidate_definition_pattern("java", "MyClass");
    let re = regex::Regex::new(&pat).expect("valid regex");
    assert!(re.is_match("public class MyClass {"));
    assert!(re.is_match("public MyClass("));
}

#[test]
fn test_candidate_definition_pattern_unknown_language() {
    let pat = super::candidate_definition_pattern("haskell", "myFunc");
    let re = regex::Regex::new(&pat).expect("valid regex");
    assert!(re.is_match("fn myFunc("));
    assert!(re.is_match("def myFunc("));
    assert!(re.is_match("function myFunc("));
    assert!(re.is_match("class myFunc {"));
}

#[test]
fn test_candidate_definition_pattern_vue_matches_ts_js() {
    let vue_pat = super::candidate_definition_pattern("vue", "handleClick");
    let ts_pat = super::candidate_definition_pattern("typescript", "handleClick");
    // Vue and typescript share the same pattern branch
    assert_eq!(vue_pat, ts_pat);
}

#[test]
fn test_candidate_definition_pattern_tsx_matches_ts() {
    let tsx_pat = super::candidate_definition_pattern("tsx", "Foo");
    let ts_pat = super::candidate_definition_pattern("typescript", "Foo");
    assert_eq!(tsx_pat, ts_pat);
}

// ── java_resolve_pattern tests ─────────────────────────────────────────

#[test]
fn test_java_resolve_pattern_compiles() {
    let pat = super::java_resolve_pattern("MyService");
    assert!(
        regex::Regex::new(&pat).is_ok(),
        "java_resolve_pattern must produce valid regex"
    );
}

#[test]
fn test_java_resolve_pattern_matches_class() {
    let pat = super::java_resolve_pattern("OrderService");
    let re = regex::Regex::new(&pat).expect("valid regex");
    assert!(re.is_match("public class OrderService {"));
    assert!(re.is_match("private interface OrderService {"));
}

// ── language_to_file_glob: edge-case coverage ──────────────────────────

#[test]
fn test_language_to_file_glob_tsx_uses_typescript_branch() {
    // "tsx" now shares the typescript branch — both use the .{ts,tsx} glob.
    let glob = super::language_to_file_glob("tsx");
    assert_eq!(glob, "**/*.{ts,tsx}");
    // Must NOT fall through to catch-all
    assert_ne!(glob, "**/*");
}

// ── last_symbol_name tests ─────────────────────────────────────────────

#[test]
fn test_last_symbol_name_returns_last_segment() {
    let sp = pathfinder_common::types::SemanticPath::parse("src/auth.rs::AuthService.login")
        .expect("valid semantic path");
    let name = super::last_symbol_name(&sp);
    assert_eq!(name, Some("login".to_string()));
}

#[test]
fn test_last_symbol_name_single_segment() {
    let sp = pathfinder_common::types::SemanticPath::parse("src/lib.rs::main")
        .expect("valid semantic path");
    let name = super::last_symbol_name(&sp);
    assert_eq!(name, Some("main".to_string()));
}

#[test]
fn test_last_symbol_name_no_symbol_chain() {
    let sp =
        pathfinder_common::types::SemanticPath::parse("src/lib.rs").expect("valid semantic path");
    let name = super::last_symbol_name(&sp);
    assert_eq!(name, None);
}

// ── PATCH-005 backward-compat: single semantic_path returns GetDefinitionResponse ──

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_locate_single_unchanged() {
    // Verify that single-path mode (no `locations`) still returns the old
    // `GetDefinitionResponse` format rather than the new `BatchLocateResult`.
    use super::test_helpers::make_server_with_lawyer;
    use crate::server::types::{GetDefinitionResponse, LocateParams};
    use pathfinder_lsp::{DefinitionLocation, MockLawyer};
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
        pathfinder_common::types::SymbolScope {
            content: "fn login() { }".to_owned(),
            start_line: 9,
            end_line: 9,
            name_column: 0,
            language: "rust".to_owned(),
        },
    ));

    let lawyer = Arc::new(MockLawyer::default());
    lawyer.push_goto_definition_result(Ok(Some(DefinitionLocation {
        file: "src/auth.rs".into(),
        line: 42,
        column: 5,
        preview: "pub fn login() -> bool {".into(),
    })));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        // locations is absent — must stay in single-path mode
        ..Default::default()
    };

    let result = server.locate_impl(params).await.expect("should succeed");
    // Response must be the legacy GetDefinitionResponse shape, NOT BatchLocateResult
    let meta: GetDefinitionResponse = serde_json::from_value(
        result
            .structured_content
            .expect("structured_content present"),
    )
    .expect("single-path mode must return GetDefinitionResponse, not BatchLocateResult");
    assert_eq!(meta.file, "src/auth.rs");
    assert_eq!(meta.line, 42);
    assert_eq!(meta.column, 5);
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_locate_batch_semantic_paths() {
    use super::test_helpers::make_server_with_lawyer;
    use crate::server::types::{BatchLocateResult, LocateEntry, LocateParams};
    use pathfinder_lsp::{DefinitionLocation, MockLawyer};
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    let surgeon = Arc::new(MockSurgeon::new());
    // inspect read_symbol_scope_enriched (needs make_scope)
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(pathfinder_common::types::SymbolScope {
            content: "fn login() { }".to_owned(),
            start_line: 9,
            end_line: 9,
            name_column: 0,
            language: "rust".to_owned(),
        }),
        Ok(pathfinder_common::types::SymbolScope {
            content: "fn main() { }".to_owned(),
            start_line: 1,
            end_line: 1,
            name_column: 0,
            language: "rust".to_owned(),
        }),
    ]);

    let lawyer = Arc::new(MockLawyer::default());
    lawyer.push_goto_definition_result(Ok(Some(DefinitionLocation {
        file: "src/auth.rs".into(),
        line: 42,
        column: 5,
        preview: "pub fn login() -> bool {".into(),
    })));
    lawyer.push_goto_definition_result(Ok(Some(DefinitionLocation {
        file: "src/main.rs".into(),
        line: 10,
        column: 1,
        preview: "fn main() {".into(),
    })));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = LocateParams {
        locations: Some(vec![
            LocateEntry {
                semantic_path: Some("src/auth.rs::login".to_owned()),
                file: None,
                line: None,
            },
            LocateEntry {
                semantic_path: Some("src/main.rs::main".to_owned()),
                file: None,
                line: None,
            },
        ]),
        ..Default::default()
    };

    let result = server.locate_impl(params).await.expect("should succeed");
    let val: BatchLocateResult = serde_json::from_value(
        result
            .structured_content
            .expect("missing structured_content"),
    )
    .expect("valid metadata");

    assert_eq!(val.succeeded, 2);
    assert_eq!(val.failed, 0);
    assert_eq!(val.results.len(), 2);
    assert_eq!(val.results[0].status, "ok");
    assert_eq!(val.results[0].file, Some("src/auth.rs".to_owned()));
    assert_eq!(val.results[0].line, Some(42));
    assert_eq!(val.results[1].status, "ok");
    assert_eq!(val.results[1].file, Some("src/main.rs".to_owned()));
    assert_eq!(val.results[1].line, Some(10));
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_locate_batch_file_line_pairs() {
    use super::test_helpers::make_server_with_lawyer;
    use crate::server::types::{BatchLocateResult, LocateEntry, LocateParams};
    use pathfinder_lsp::MockLawyer;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .extend([Ok(Some("login".to_owned())), Ok(Some("main".to_owned()))]);

    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = LocateParams {
        locations: Some(vec![
            LocateEntry {
                semantic_path: None,
                file: Some("src/auth.rs".to_owned()),
                line: Some(9),
            },
            LocateEntry {
                semantic_path: None,
                file: Some("src/main.rs".to_owned()),
                line: Some(1),
            },
        ]),
        ..Default::default()
    };

    let result = server.locate_impl(params).await.expect("should succeed");
    let val: BatchLocateResult = serde_json::from_value(
        result
            .structured_content
            .expect("missing structured_content"),
    )
    .expect("valid metadata");

    assert_eq!(val.succeeded, 2);
    assert_eq!(val.failed, 0);
    assert_eq!(val.results.len(), 2);
    assert_eq!(val.results[0].status, "ok");
    assert_eq!(
        val.results[0].semantic_path,
        Some("src/auth.rs::login".to_owned())
    );
    assert_eq!(val.results[1].status, "ok");
    assert_eq!(
        val.results[1].semantic_path,
        Some("src/main.rs::main".to_owned())
    );
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_locate_batch_mixed_modes() {
    use super::test_helpers::make_server_with_lawyer;
    use crate::server::types::{BatchLocateResult, LocateEntry, LocateParams};
    use pathfinder_lsp::{DefinitionLocation, MockLawyer};
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .extend([Ok(pathfinder_common::types::SymbolScope {
            content: "fn login() { }".to_owned(),
            start_line: 9,
            end_line: 9,
            name_column: 0,
            language: "rust".to_owned(),
        })]);
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .extend([Ok(Some("main".to_owned()))]);

    let lawyer = Arc::new(MockLawyer::default());
    lawyer.push_goto_definition_result(Ok(Some(DefinitionLocation {
        file: "src/auth.rs".into(),
        line: 42,
        column: 5,
        preview: "pub fn login() -> bool {".into(),
    })));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = LocateParams {
        locations: Some(vec![
            LocateEntry {
                semantic_path: Some("src/auth.rs::login".to_owned()),
                file: None,
                line: None,
            },
            LocateEntry {
                semantic_path: None,
                file: Some("src/main.rs".to_owned()),
                line: Some(1),
            },
        ]),
        ..Default::default()
    };

    let result = server.locate_impl(params).await.expect("should succeed");
    let val: BatchLocateResult = serde_json::from_value(
        result
            .structured_content
            .expect("missing structured_content"),
    )
    .expect("valid metadata");

    assert_eq!(val.succeeded, 2);
    assert_eq!(val.failed, 0);
    assert_eq!(val.results.len(), 2);
    assert_eq!(val.results[0].status, "ok");
    assert_eq!(val.results[0].file, Some("src/auth.rs".to_owned()));
    assert_eq!(val.results[0].line, Some(42));
    assert_eq!(val.results[1].status, "ok");
    assert_eq!(
        val.results[1].semantic_path,
        Some("src/main.rs::main".to_owned())
    );
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_locate_batch_partial_failure() {
    use super::test_helpers::make_server_with_lawyer;
    use crate::server::types::{BatchLocateResult, LocateEntry, LocateParams};
    use pathfinder_lsp::MockLawyer;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .extend([Ok(Some("main".to_owned()))]);

    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = LocateParams {
        locations: Some(vec![
            LocateEntry {
                semantic_path: None,
                file: Some("src/main.rs".to_owned()),
                line: Some(1),
            },
            LocateEntry {
                semantic_path: None,
                file: Some("nonexistent.rs".to_owned()),
                line: Some(5),
            },
        ]),
        ..Default::default()
    };

    let result = server.locate_impl(params).await.expect("should succeed");
    let val: BatchLocateResult = serde_json::from_value(
        result
            .structured_content
            .expect("missing structured_content"),
    )
    .expect("valid metadata");

    assert_eq!(val.succeeded, 1);
    assert_eq!(val.failed, 1);
    assert_eq!(val.results.len(), 2);
    assert_eq!(val.results[0].status, "ok");
    assert_eq!(
        val.results[0].semantic_path,
        Some("src/main.rs::main".to_owned())
    );
    assert_eq!(val.results[1].status, "error");
    assert!(val.results[1].error.is_some());
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_locate_batch_max_10_limit() {
    use super::test_helpers::make_server_with_lawyer;
    use crate::server::types::{LocateEntry, LocateParams};
    use pathfinder_lsp::MockLawyer;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let entries = (1..=11)
        .map(|i| LocateEntry {
            semantic_path: None,
            file: Some(format!("src/file{i}.rs")),
            line: Some(1),
        })
        .collect();

    let params = LocateParams {
        locations: Some(entries),
        ..Default::default()
    };

    let result = server.locate_impl(params).await;
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_locate_batch_mutual_exclusion_both_params_errors() {
    // Providing both `locations` and single-mode params simultaneously must return INVALID_PARAMS.
    use super::test_helpers::make_server_with_lawyer;
    use crate::server::types::{LocateEntry, LocateParams};
    use pathfinder_lsp::MockLawyer;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = LocateParams {
        // batch param
        locations: Some(vec![LocateEntry {
            semantic_path: Some("src/foo.rs::bar".to_owned()),
            file: None,
            line: None,
        }]),
        // single-mode param present at the same time → mutual exclusion must fire
        semantic_path: Some("src/foo.rs::bar".to_owned()),
        ..Default::default()
    };

    let result = server.locate_impl(params).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(
        err.code,
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "both locations and single-mode params must return INVALID_PARAMS, got: {:?}",
        err.message
    );
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_locate_batch_empty_returns_error() {
    use super::test_helpers::make_server_with_lawyer;
    use crate::server::types::LocateParams;
    use pathfinder_lsp::MockLawyer;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = LocateParams {
        locations: Some(vec![]),
        ..Default::default()
    };

    let result = server.locate_impl(params).await;
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );
}

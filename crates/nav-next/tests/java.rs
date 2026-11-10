use nav_next::{FileId, IndexBuilder, JavaAnalyzer, Position, QueryEngine, SymbolKind};

const INTERFACE: &str = r#"
package demo.api;

public interface UserService {
    String findById(int id);
}
"#;

const IMPLEMENTATION: &str = r#"
package demo.impl;

import demo.api.UserService;

public class UserServiceImpl implements UserService {
    @Override
    public String findById(int id) {
        return "x";
    }

    public String callIt(int id) {
        return findById(id);
    }
}
"#;

#[test]
fn java_analyzer_extracts_types_methods_and_references() {
    let analyzer = JavaAnalyzer::new();
    let interface = analyzer.analyze(FileId(1), INTERFACE).unwrap();
    let implementation = analyzer.analyze(FileId(2), IMPLEMENTATION).unwrap();

    assert!(interface.symbols.iter().any(|symbol| {
        symbol.kind == SymbolKind::Interface && symbol.fqn == "demo.api.UserService"
    }));
    assert!(interface.symbols.iter().any(|symbol| {
        symbol.kind == SymbolKind::Method && symbol.fqn == "demo.api.UserService.findById"
    }));

    assert!(implementation.symbols.iter().any(|symbol| {
        symbol.kind == SymbolKind::Class && symbol.fqn == "demo.impl.UserServiceImpl"
    }));
    assert!(implementation.symbols.iter().any(|symbol| {
        symbol.kind == SymbolKind::Method && symbol.fqn == "demo.impl.UserServiceImpl.findById"
    }));
    assert!(implementation
        .unresolved_type_edges
        .iter()
        .any(|edge| edge.parent_name == "UserService"));
    assert!(implementation
        .occurrences
        .iter()
        .any(|occurrence| occurrence.file_id == FileId(2)));
}

#[test]
fn java_analyzer_supports_implementation_and_supertype_queries() {
    let analyzer = JavaAnalyzer::new();
    let interface = analyzer.analyze(FileId(1), INTERFACE).unwrap();
    let implementation = analyzer.analyze(FileId(2), IMPLEMENTATION).unwrap();

    let mut builder = IndexBuilder::new();
    builder.add_file(interface).unwrap();
    builder.add_file(implementation).unwrap();
    let snapshot = builder.build().unwrap();
    let query = QueryEngine::new(snapshot);

    let interface_method = query
        .snapshot()
        .symbol_by_fqn("demo.api.UserService.findById")
        .first()
        .map(|symbol| symbol.id)
        .expect("interface method");
    let implementations = query.implementations(interface_method);
    assert_eq!(implementations.len(), 1);
    assert_eq!(implementations[0].fqn, "demo.impl.UserServiceImpl.findById");

    let supertypes = query.supertypes(implementations[0].id);
    assert_eq!(supertypes.len(), 1);
    assert_eq!(supertypes[0].fqn, "demo.api.UserService.findById");
}

#[test]
fn java_analyzer_resolves_same_file_method_calls() {
    let analyzer = JavaAnalyzer::new();
    let interface = analyzer.analyze(FileId(1), INTERFACE).unwrap();
    let implementation = analyzer.analyze(FileId(2), IMPLEMENTATION).unwrap();

    let mut builder = IndexBuilder::new();
    builder.add_file(interface).unwrap();
    builder.add_file(implementation).unwrap();
    let snapshot = builder.build().unwrap();
    let query = QueryEngine::new(snapshot);

    let method = query
        .snapshot()
        .symbol_by_fqn("demo.impl.UserServiceImpl.findById")
        .first()
        .map(|symbol| symbol.id)
        .expect("implementation method");
    let references = query.references(method);
    assert!(references
        .iter()
        .any(|occurrence| occurrence.file_id == FileId(2)));

    let interface_method = query
        .snapshot()
        .symbol_by_fqn("demo.api.UserService.findById")
        .first()
        .map(|symbol| symbol.id)
        .expect("interface method");
    let search = query.search_methods("demo.api.UserService#findById", 10);
    assert_eq!(search.len(), 1);
    assert_eq!(search[0].id, interface_method);
}

#[test]
fn java_analyzer_binds_method_call_to_receiver_type() {
    let controller = r#"
package demo.web;

public class AuthController {
    private final RegisterService registerService;

    public Object register(Object request) {
        return registerService.register(request);
    }
}
"#;
    let service = r#"
package demo.service;

public class RegisterService {
    public Object register(Object request) {
        return request;
    }
}
"#;

    let analyzer = JavaAnalyzer::new();
    let controller_contribution = analyzer.analyze(FileId(1), controller).unwrap();
    let service_contribution = analyzer.analyze(FileId(2), service).unwrap();

    let mut builder = IndexBuilder::new();
    builder.add_file(controller_contribution).unwrap();
    builder.add_file(service_contribution).unwrap();
    let snapshot = builder.build().unwrap();
    let query = QueryEngine::new(snapshot);

    let mut position = position_of(controller, ".register(");
    position.character += 1;
    let definitions = query.definition_at(FileId(1), position);
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].file_id, FileId(2));
}

#[test]
fn java_analyzer_binds_receiver_variable_to_field_declaration() {
    let controller = r#"
package demo.web;

public class AuthController {
    private final RegisterService registerService;

    public Object register(Object request) {
        return registerService.register(request);
    }
}
"#;

    let analyzer = JavaAnalyzer::new();
    let contribution = analyzer.analyze(FileId(1), controller).unwrap();

    let mut builder = IndexBuilder::new();
    builder.add_file(contribution).unwrap();
    let snapshot = builder.build().unwrap();
    let query = QueryEngine::new(snapshot);

    let position = position_of(controller, "registerService.register");
    let symbol_id = query
        .symbol_at(FileId(1), position)
        .expect("receiver variable symbol");
    let symbol = query.snapshot().symbol(symbol_id).expect("field symbol");

    assert_eq!(symbol.kind, SymbolKind::Field);
    assert_eq!(symbol.name, "registerService");
}

#[test]
fn java_analyzer_binds_static_method_receiver_and_method() {
    let user = r#"
package demo.domain;

public class User {
    public static User create(String username) {
        return new User();
    }
}
"#;
    let service = r#"
package demo.service;

import demo.domain.User;

public class RegisterService {
    public User register() {
        return User.create("x");
    }
}
"#;

    let analyzer = JavaAnalyzer::new();
    let user_contribution = analyzer.analyze(FileId(1), user).unwrap();
    let service_contribution = analyzer.analyze(FileId(2), service).unwrap();

    let mut builder = IndexBuilder::new();
    builder.add_file(user_contribution).unwrap();
    builder.add_file(service_contribution).unwrap();
    let snapshot = builder.build().unwrap();
    let query = QueryEngine::new(snapshot);

    let receiver_position = position_of(service, "User.create");
    let receiver_definitions = query.definition_at(FileId(2), receiver_position);
    assert_eq!(receiver_definitions.len(), 1);
    assert_eq!(receiver_definitions[0].file_id, FileId(1));

    let method_position = position_of(service, ".create(");
    let method_position = Position {
        line: method_position.line,
        character: method_position.character + 1,
    };
    let method_definitions = query.definition_at(FileId(2), method_position);
    assert_eq!(method_definitions.len(), 1);
    assert_eq!(method_definitions[0].file_id, FileId(1));
}

#[test]
fn java_analyzer_binds_local_variables_and_parameters() {
    let source = r#"
package demo;

public class Service {
    public Object register(Object request) {
        Object user = request;
        return user;
    }
}
"#;

    let analyzer = JavaAnalyzer::new();
    let contribution = analyzer.analyze(FileId(1), source).unwrap();

    let mut builder = IndexBuilder::new();
    builder.add_file(contribution).unwrap();
    let snapshot = builder.build().unwrap();
    let query = QueryEngine::new(snapshot);

    let user_position = position_of(source, "return user;");
    let user_symbol = query
        .symbol_at(
            FileId(1),
            Position {
                line: user_position.line,
                character: user_position.character + 7,
            },
        )
        .expect("local variable symbol");
    let user = query.snapshot().symbol(user_symbol).unwrap();
    assert_eq!(user.kind, SymbolKind::Local);
    assert_eq!(user.name, "user");

    let request_position = position_of(source, "= request;");
    let request_symbol = query
        .symbol_at(
            FileId(1),
            Position {
                line: request_position.line,
                character: request_position.character + 2,
            },
        )
        .expect("parameter symbol");
    let request = query.snapshot().symbol(request_symbol).unwrap();
    assert_eq!(request.kind, SymbolKind::Parameter);
    assert_eq!(request.name, "request");
}

#[test]
fn java_analyzer_uses_utf16_positions() {
    let analyzer = JavaAnalyzer::new();
    let source = "package demo;\r\npublic class A {}";
    let contribution = analyzer.analyze(FileId(1), source).unwrap();
    let symbol = contribution
        .symbols
        .iter()
        .find(|symbol| symbol.name == "A")
        .expect("class A");
    assert!(symbol.selection_span.start.line >= 1);
    assert!(symbol.selection_span.contains(Position {
        line: 1,
        character: 13,
    }));
}

fn position_of(source: &str, needle: &str) -> Position {
    let index = source
        .find(needle)
        .unwrap_or_else(|| panic!("cannot find {needle}"));
    let before = &source[..index];
    let line = before.matches('\n').count() as u32;
    let line_start = before.rfind('\n').map(|index| index + 1).unwrap_or(0);
    let character = source[line_start..index].encode_utf16().count() as u32;
    Position { line, character }
}

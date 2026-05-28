use super::*;
use std::path::PathBuf;

fn pub_trait_spec(path: &str, method: &str) -> PublisherSpec {
    PublisherSpec {
        kind: PublisherKind::Trait,
        path: Some(path.to_string()),
        type_path: None,
        method: method.to_string(),
        subject_arg_index: 0,
    }
}

fn pub_inherent_spec(type_path: &str, method: &str, arg_idx: usize) -> PublisherSpec {
    PublisherSpec {
        kind: PublisherKind::Inherent,
        path: None,
        type_path: Some(type_path.to_string()),
        method: method.to_string(),
        subject_arg_index: arg_idx,
    }
}

fn sub_trait_spec(path: &str, method: &str) -> ConsumerSpec {
    ConsumerSpec {
        kind: PublisherKind::Trait,
        path: Some(path.to_string()),
        type_path: None,
        method: method.to_string(),
        subject_arg_index: 0,
        consumer_name_arg_index: Some(1),
    }
}

fn extract_with(spec: &CallSiteConfig, src: &str) -> PerFileCallSites {
    extract(&PathBuf::from("test.rs"), src, "test-crate", spec).expect("parse")
}

#[test]
fn matches_trait_publish_with_import() {
    let src = r#"
        use my_bus::Publisher;
        async fn run(p: &dyn Publisher) {
            p.publish("foo.bar", &[]).await.unwrap();
        }
    "#;
    let cfg = CallSiteConfig {
        method_match: MethodMatch::NameTraitPathHint,
        publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
        consumers: vec![],
    };
    let out = extract_with(&cfg, src);
    assert_eq!(out.sites.len(), 1);
    assert_eq!(out.sites[0].kind, CallKind::Publish);
    assert!(out.sites[0].subject_expr.contains("foo.bar"));
}

#[test]
fn rejects_trait_publish_without_import() {
    let src = r#"
        async fn run(p: &dyn SomethingElse) {
            p.publish("foo.bar", &[]).await.unwrap();
        }
    "#;
    let cfg = CallSiteConfig {
        method_match: MethodMatch::NameTraitPathHint,
        publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
        consumers: vec![],
    };
    let out = extract_with(&cfg, src);
    assert!(out.sites.is_empty(), "no use -> no match under hint");
}

/// Critical regression for synthesis H1: two `.publish()` methods
/// in different crates, only the configured one should match.
#[test]
fn false_positive_method_name_filtered_by_import_hint() {
    // Mimics Gordon's gordon_platform::ipc::PublishableChannel
    // (wrong trait) vs gordon_bus::Publisher (right trait).
    let src = r"
        use gordon_platform::ipc::Publisher as IpcPublisher;

        async fn run(p: &dyn IpcPublisher) {
            p.publish(SomeChannel::Foo, record).await.unwrap();
        }
    ";
    let cfg = CallSiteConfig {
        method_match: MethodMatch::NameTraitPathHint,
        publishers: vec![pub_trait_spec("gordon_bus::Publisher", "publish")],
        consumers: vec![],
    };
    let out = extract_with(&cfg, src);
    assert!(
        out.sites.is_empty(),
        "ipc::Publisher publish must not match gordon_bus::Publisher"
    );
}

/// Path-expression subject (enum variant) gets rejected by
/// the arg-shape filter even when method name matches.
#[test]
fn enum_variant_subject_arg_rejected() {
    let src = r"
        use my_bus::Publisher;
        async fn run(p: &dyn Publisher) {
            p.publish(Channel::BotEvents, &[]).await.unwrap();
        }
    ";
    let cfg = CallSiteConfig {
        method_match: MethodMatch::NameTraitPathHint,
        publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
        consumers: vec![],
    };
    let out = extract_with(&cfg, src);
    assert!(out.sites.is_empty(), "PascalCase variant rejected");
}

#[test]
fn matches_inherent_publish_with_arg_index_one() {
    let src = r"
        use my_bus::nats::NatsPublisher;
        async fn run() {
            NatsPublisher::publish_within(&mut tx, &subject, &[]).await.unwrap();
        }
    ";
    let cfg = CallSiteConfig {
        method_match: MethodMatch::NameTraitPathHint,
        publishers: vec![pub_inherent_spec(
            "my_bus::nats::NatsPublisher",
            "publish_within",
            1,
        )],
        consumers: vec![],
    };
    let out = extract_with(&cfg, src);
    assert_eq!(out.sites.len(), 1, "inherent publish_within matched");
    assert!(out.sites[0].subject_expr.contains("subject"));
}

#[test]
fn const_subject_accepted_as_string_shaped() {
    let src = r#"
        use my_bus::Publisher;
        pub const SUBJECT: &str = "foo.bar";
        async fn run(p: &dyn Publisher) {
            p.publish(SUBJECT, &[]).await.unwrap();
        }
    "#;
    let cfg = CallSiteConfig {
        method_match: MethodMatch::NameTraitPathHint,
        publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
        consumers: vec![],
    };
    let out = extract_with(&cfg, src);
    assert_eq!(out.sites.len(), 1);
    let const_count = out
        .symbols
        .symbols
        .iter()
        .filter(|(_, s)| matches!(s, Symbol::Const(_)))
        .count();
    assert_eq!(const_count, 1, "const recorded as Symbol");
}

#[test]
fn format_subject_accepted() {
    let src = r#"
        use my_bus::Publisher;
        async fn run(p: &dyn Publisher, sym: &str) {
            p.publish(format!("market.klines.{sym}.1m"), &[]).await.unwrap();
        }
    "#;
    let cfg = CallSiteConfig {
        method_match: MethodMatch::NameTraitPathHint,
        publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
        consumers: vec![],
    };
    let out = extract_with(&cfg, src);
    assert_eq!(out.sites.len(), 1);
}

#[test]
fn reference_to_const_accepted() {
    let src = r#"
        use my_bus::nats::NatsPublisher;
        pub const SUBJECT: &str = "foo";
        async fn run() {
            NatsPublisher::publish_within(&mut tx, &SUBJECT, &[]).await.unwrap();
        }
    "#;
    let cfg = CallSiteConfig {
        method_match: MethodMatch::NameTraitPathHint,
        publishers: vec![pub_inherent_spec(
            "my_bus::nats::NatsPublisher",
            "publish_within",
            1,
        )],
        consumers: vec![],
    };
    let out = extract_with(&cfg, src);
    assert_eq!(out.sites.len(), 1);
    assert!(out.sites[0].subject_expr.contains("SUBJECT"));
}

#[test]
fn subscribe_captures_consumer_name() {
    let src = r#"
        use my_bus::Consumer;
        async fn run(c: &dyn Consumer) {
            c.subscribe("foo.bar", "my-durable").await.unwrap();
        }
    "#;
    let cfg = CallSiteConfig {
        method_match: MethodMatch::NameTraitPathHint,
        publishers: vec![],
        consumers: vec![sub_trait_spec("my_bus::Consumer", "subscribe")],
    };
    let out = extract_with(&cfg, src);
    assert_eq!(out.sites.len(), 1);
    assert_eq!(out.sites[0].kind, CallKind::Subscribe);
    assert!(out.sites[0]
        .consumer_name_expr
        .as_deref()
        .unwrap()
        .contains("my-durable"));
}

#[test]
fn extracts_free_function_into_symbols() {
    let src = r#"
        pub fn build_breaker_subject(name: &str) -> String {
            format!("risk.events.{name}")
        }
    "#;
    let cfg = CallSiteConfig::default();
    let out = extract_with(&cfg, src);
    assert!(out.sites.is_empty());
    assert_eq!(out.symbols.symbols.len(), 1);
    match &out.symbols.symbols[0] {
        (path, Symbol::Function(snip)) => {
            assert!(path.ends_with("::build_breaker_subject"));
            assert!(snip.source.contains("risk.events"));
        }
        other => panic!("expected Function, got {other:?}"),
    }
}

#[test]
fn extracts_impl_methods_into_symbols() {
    let src = r#"
        pub struct FillEvent;
        impl FillEvent {
            pub fn nats_subject(&self) -> String {
                format!("trading.fills.{}", "bot")
            }
        }
    "#;
    let cfg = CallSiteConfig::default();
    let out = extract_with(&cfg, src);
    let method = out
        .symbols
        .symbols
        .iter()
        .find(|(_, s)| matches!(s, Symbol::Method { .. }));
    assert!(method.is_some(), "impl method recorded");
}

#[test]
fn sites_sorted_by_position() {
    let src = r#"
        use my_bus::Publisher;
        async fn run(p: &dyn Publisher) {
            p.publish("third.subject", &[]).await.unwrap();
            p.publish("first.subject", &[]).await.unwrap();
            p.publish("second.subject", &[]).await.unwrap();
        }
    "#;
    let cfg = CallSiteConfig {
        method_match: MethodMatch::NameTraitPathHint,
        publishers: vec![pub_trait_spec("my_bus::Publisher", "publish")],
        consumers: vec![],
    };
    let out = extract_with(&cfg, src);
    assert_eq!(out.sites.len(), 3);
    // Stable: first site is line N, third site is line N+2.
    assert!(out.sites[0].line < out.sites[1].line);
    assert!(out.sites[1].line < out.sites[2].line);
    assert!(out.sites[0].subject_expr.contains("third.subject"));
    assert!(out.sites[2].subject_expr.contains("second.subject"));
}

#[test]
fn parse_error_returns_typed_err() {
    let res = extract(
        &PathBuf::from("bad.rs"),
        "this is not valid rust ::: {",
        "x",
        &CallSiteConfig::default(),
    );
    assert!(matches!(res, Err(ParseError::Syn { .. })));
}

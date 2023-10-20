use crate::SharedString;
use anyhow::{anyhow, Result};
use collections::{HashMap, HashSet};
use std::any::Any;

pub trait Action: Any + Send + Sync {
    fn partial_eq(&self, action: &dyn Action) -> bool;
    fn boxed_clone(&self) -> Box<dyn Action>;
    fn as_any(&self) -> &dyn Any;
}

impl<T> Action for T
where
    T: Any + PartialEq + Clone + Send + Sync,
{
    fn partial_eq(&self, action: &dyn Action) -> bool {
        action
            .as_any()
            .downcast_ref::<Self>()
            .map_or(false, |a| self == a)
    }

    fn boxed_clone(&self) -> Box<dyn Action> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DispatchContext {
    set: HashSet<SharedString>,
    map: HashMap<SharedString, SharedString>,
}

impl<'a> TryFrom<&'a str> for DispatchContext {
    type Error = anyhow::Error;

    fn try_from(value: &'a str) -> Result<Self> {
        Self::parse(value)
    }
}

impl DispatchContext {
    pub fn parse(source: &str) -> Result<Self> {
        let mut context = Self::default();
        let source = skip_whitespace(source);
        Self::parse_expr(&source, &mut context)?;
        Ok(context)
    }

    fn parse_expr(mut source: &str, context: &mut Self) -> Result<()> {
        if source.is_empty() {
            return Ok(());
        }

        let key = source
            .chars()
            .take_while(|c| is_identifier_char(*c))
            .collect::<String>();
        source = skip_whitespace(&source[key.len()..]);
        if let Some(suffix) = source.strip_prefix('=') {
            source = skip_whitespace(suffix);
            let value = source
                .chars()
                .take_while(|c| is_identifier_char(*c))
                .collect::<String>();
            source = skip_whitespace(&source[value.len()..]);
            context.set(key, value);
        } else {
            context.insert(key);
        }

        Self::parse_expr(source, context)
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.map.is_empty()
    }

    pub fn clear(&mut self) {
        self.set.clear();
        self.map.clear();
    }

    pub fn extend(&mut self, other: &Self) {
        for v in &other.set {
            self.set.insert(v.clone());
        }
        for (k, v) in &other.map {
            self.map.insert(k.clone(), v.clone());
        }
    }

    pub fn insert<I: Into<SharedString>>(&mut self, identifier: I) {
        self.set.insert(identifier.into());
    }

    pub fn set<S1: Into<SharedString>, S2: Into<SharedString>>(&mut self, key: S1, value: S2) {
        self.map.insert(key.into(), value.into());
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum DispatchContextPredicate {
    Identifier(SharedString),
    Equal(SharedString, SharedString),
    NotEqual(SharedString, SharedString),
    Child(Box<DispatchContextPredicate>, Box<DispatchContextPredicate>),
    Not(Box<DispatchContextPredicate>),
    And(Box<DispatchContextPredicate>, Box<DispatchContextPredicate>),
    Or(Box<DispatchContextPredicate>, Box<DispatchContextPredicate>),
}

impl DispatchContextPredicate {
    pub fn parse(source: &str) -> Result<Self> {
        let source = skip_whitespace(source);
        let (predicate, rest) = Self::parse_expr(source, 0)?;
        if let Some(next) = rest.chars().next() {
            Err(anyhow!("unexpected character {next:?}"))
        } else {
            Ok(predicate)
        }
    }

    pub fn eval(&self, contexts: &[&DispatchContext]) -> bool {
        let Some(context) = contexts.last() else {
            return false;
        };
        match self {
            Self::Identifier(name) => context.set.contains(&name),
            Self::Equal(left, right) => context
                .map
                .get(&left)
                .map(|value| value == right)
                .unwrap_or(false),
            Self::NotEqual(left, right) => context
                .map
                .get(&left)
                .map(|value| value != right)
                .unwrap_or(true),
            Self::Not(pred) => !pred.eval(contexts),
            Self::Child(parent, child) => {
                parent.eval(&contexts[..contexts.len() - 1]) && child.eval(contexts)
            }
            Self::And(left, right) => left.eval(contexts) && right.eval(contexts),
            Self::Or(left, right) => left.eval(contexts) || right.eval(contexts),
        }
    }

    fn parse_expr(mut source: &str, min_precedence: u32) -> anyhow::Result<(Self, &str)> {
        type Op = fn(
            DispatchContextPredicate,
            DispatchContextPredicate,
        ) -> Result<DispatchContextPredicate>;

        let (mut predicate, rest) = Self::parse_primary(source)?;
        source = rest;

        'parse: loop {
            for (operator, precedence, constructor) in [
                (">", PRECEDENCE_CHILD, Self::new_child as Op),
                ("&&", PRECEDENCE_AND, Self::new_and as Op),
                ("||", PRECEDENCE_OR, Self::new_or as Op),
                ("==", PRECEDENCE_EQ, Self::new_eq as Op),
                ("!=", PRECEDENCE_EQ, Self::new_neq as Op),
            ] {
                if source.starts_with(operator) && precedence >= min_precedence {
                    source = skip_whitespace(&source[operator.len()..]);
                    let (right, rest) = Self::parse_expr(source, precedence + 1)?;
                    predicate = constructor(predicate, right)?;
                    source = rest;
                    continue 'parse;
                }
            }
            break;
        }

        Ok((predicate, source))
    }

    fn parse_primary(mut source: &str) -> anyhow::Result<(Self, &str)> {
        let next = source
            .chars()
            .next()
            .ok_or_else(|| anyhow!("unexpected eof"))?;
        match next {
            '(' => {
                source = skip_whitespace(&source[1..]);
                let (predicate, rest) = Self::parse_expr(source, 0)?;
                if rest.starts_with(')') {
                    source = skip_whitespace(&rest[1..]);
                    Ok((predicate, source))
                } else {
                    Err(anyhow!("expected a ')'"))
                }
            }
            '!' => {
                let source = skip_whitespace(&source[1..]);
                let (predicate, source) = Self::parse_expr(&source, PRECEDENCE_NOT)?;
                Ok((DispatchContextPredicate::Not(Box::new(predicate)), source))
            }
            _ if is_identifier_char(next) => {
                let len = source
                    .find(|c: char| !is_identifier_char(c))
                    .unwrap_or(source.len());
                let (identifier, rest) = source.split_at(len);
                source = skip_whitespace(rest);
                Ok((
                    DispatchContextPredicate::Identifier(identifier.to_string().into()),
                    source,
                ))
            }
            _ => Err(anyhow!("unexpected character {next:?}")),
        }
    }

    fn new_or(self, other: Self) -> Result<Self> {
        Ok(Self::Or(Box::new(self), Box::new(other)))
    }

    fn new_and(self, other: Self) -> Result<Self> {
        Ok(Self::And(Box::new(self), Box::new(other)))
    }

    fn new_child(self, other: Self) -> Result<Self> {
        Ok(Self::Child(Box::new(self), Box::new(other)))
    }

    fn new_eq(self, other: Self) -> Result<Self> {
        if let (Self::Identifier(left), Self::Identifier(right)) = (self, other) {
            Ok(Self::Equal(left, right))
        } else {
            Err(anyhow!("operands must be identifiers"))
        }
    }

    fn new_neq(self, other: Self) -> Result<Self> {
        if let (Self::Identifier(left), Self::Identifier(right)) = (self, other) {
            Ok(Self::NotEqual(left, right))
        } else {
            Err(anyhow!("operands must be identifiers"))
        }
    }
}

const PRECEDENCE_CHILD: u32 = 1;
const PRECEDENCE_OR: u32 = 2;
const PRECEDENCE_AND: u32 = 3;
const PRECEDENCE_EQ: u32 = 4;
const PRECEDENCE_NOT: u32 = 5;

fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

fn skip_whitespace(source: &str) -> &str {
    let len = source
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(source.len());
    &source[len..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use DispatchContextPredicate::*;

    #[test]
    fn test_parse_context() {
        let mut expected = DispatchContext::default();
        expected.set("foo", "bar");
        expected.insert("baz");
        assert_eq!(DispatchContext::parse("baz foo=bar").unwrap(), expected);
        assert_eq!(DispatchContext::parse("foo = bar baz").unwrap(), expected);
        assert_eq!(
            DispatchContext::parse("  baz foo   =   bar baz").unwrap(),
            expected
        );
        assert_eq!(DispatchContext::parse(" foo = bar baz").unwrap(), expected);
    }

    #[test]
    fn test_parse_identifiers() {
        // Identifiers
        assert_eq!(
            DispatchContextPredicate::parse("abc12").unwrap(),
            Identifier("abc12".into())
        );
        assert_eq!(
            DispatchContextPredicate::parse("_1a").unwrap(),
            Identifier("_1a".into())
        );
    }

    #[test]
    fn test_parse_negations() {
        assert_eq!(
            DispatchContextPredicate::parse("!abc").unwrap(),
            Not(Box::new(Identifier("abc".into())))
        );
        assert_eq!(
            DispatchContextPredicate::parse(" ! ! abc").unwrap(),
            Not(Box::new(Not(Box::new(Identifier("abc".into())))))
        );
    }

    #[test]
    fn test_parse_equality_operators() {
        assert_eq!(
            DispatchContextPredicate::parse("a == b").unwrap(),
            Equal("a".into(), "b".into())
        );
        assert_eq!(
            DispatchContextPredicate::parse("c!=d").unwrap(),
            NotEqual("c".into(), "d".into())
        );
        assert_eq!(
            DispatchContextPredicate::parse("c == !d")
                .unwrap_err()
                .to_string(),
            "operands must be identifiers"
        );
    }

    #[test]
    fn test_parse_boolean_operators() {
        assert_eq!(
            DispatchContextPredicate::parse("a || b").unwrap(),
            Or(
                Box::new(Identifier("a".into())),
                Box::new(Identifier("b".into()))
            )
        );
        assert_eq!(
            DispatchContextPredicate::parse("a || !b && c").unwrap(),
            Or(
                Box::new(Identifier("a".into())),
                Box::new(And(
                    Box::new(Not(Box::new(Identifier("b".into())))),
                    Box::new(Identifier("c".into()))
                ))
            )
        );
        assert_eq!(
            DispatchContextPredicate::parse("a && b || c&&d").unwrap(),
            Or(
                Box::new(And(
                    Box::new(Identifier("a".into())),
                    Box::new(Identifier("b".into()))
                )),
                Box::new(And(
                    Box::new(Identifier("c".into())),
                    Box::new(Identifier("d".into()))
                ))
            )
        );
        assert_eq!(
            DispatchContextPredicate::parse("a == b && c || d == e && f").unwrap(),
            Or(
                Box::new(And(
                    Box::new(Equal("a".into(), "b".into())),
                    Box::new(Identifier("c".into()))
                )),
                Box::new(And(
                    Box::new(Equal("d".into(), "e".into())),
                    Box::new(Identifier("f".into()))
                ))
            )
        );
        assert_eq!(
            DispatchContextPredicate::parse("a && b && c && d").unwrap(),
            And(
                Box::new(And(
                    Box::new(And(
                        Box::new(Identifier("a".into())),
                        Box::new(Identifier("b".into()))
                    )),
                    Box::new(Identifier("c".into())),
                )),
                Box::new(Identifier("d".into()))
            ),
        );
    }

    #[test]
    fn test_parse_parenthesized_expressions() {
        assert_eq!(
            DispatchContextPredicate::parse("a && (b == c || d != e)").unwrap(),
            And(
                Box::new(Identifier("a".into())),
                Box::new(Or(
                    Box::new(Equal("b".into(), "c".into())),
                    Box::new(NotEqual("d".into(), "e".into())),
                )),
            ),
        );
        assert_eq!(
            DispatchContextPredicate::parse(" ( a || b ) ").unwrap(),
            Or(
                Box::new(Identifier("a".into())),
                Box::new(Identifier("b".into())),
            )
        );
    }
}
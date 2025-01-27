use safecast::TryCastInto;

use tc_error::*;
use tc_value::{Number, NumberClass, NumberInstance, Trigonometry, Value};
use tcgeneric::{label, PathSegment};

use crate::route::{GetHandler, Handler, PostHandler, Route};
use crate::state::State;

struct Dual<F> {
    op: F,
}

impl<F> Dual<F> {
    fn new(op: F) -> Self {
        Self { op }
    }
}

impl<'a, F> Handler<'a> for Dual<F>
where
    F: Fn(Number) -> TCResult<Number> + Send + 'a,
{
    fn get<'b>(self: Box<Self>) -> Option<GetHandler<'a, 'b>>
    where
        'b: 'a,
    {
        Some(Box::new(|_txn, value| {
            Box::pin(async move {
                let value = value.try_cast_into(|v| TCError::bad_request("not a Number", v))?;
                (self.op)(value).map(Value::Number).map(State::from)
            })
        }))
    }

    fn post<'b>(self: Box<Self>) -> Option<PostHandler<'a, 'b>>
    where
        'b: 'a,
    {
        Some(Box::new(|_txn, mut params| {
            Box::pin(async move {
                let value: Number = params.require(&label("r").into())?;
                params.expect_empty()?;

                (self.op)(value).map(Value::Number).map(State::from)
            })
        }))
    }
}

struct Unary<F> {
    name: &'static str,
    op: F,
}

impl<F> Unary<F> {
    fn new(name: &'static str, op: F) -> Self {
        Self { name, op }
    }
}

impl<'a, F> Handler<'a> for Unary<F>
where
    F: Fn() -> Number + Send + 'a,
{
    fn get<'b>(self: Box<Self>) -> Option<GetHandler<'a, 'b>>
    where
        'b: 'a,
    {
        Some(Box::new(|_txn, value| {
            Box::pin(async move {
                if value.is_some() {
                    return Err(TCError::unsupported(format!(
                        "{} does not have any parameters (found {})",
                        self.name, value
                    )));
                }

                Ok(State::from(Value::from((self.op)())))
            })
        }))
    }
}

impl Route for Number {
    fn route<'a>(&'a self, path: &'a [PathSegment]) -> Option<Box<dyn Handler<'a> + 'a>> {
        if path.len() != 1 {
            return None;
        }

        let handler: Box<dyn Handler<'a> + 'a> = match path[0].as_str() {
            // basic math
            "abs" => Box::new(Unary::new("abs", move || self.abs())),
            "add" => Box::new(Dual::new(move |other| Ok(*self + other))),
            "and" => Box::new(Dual::new(move |other| Ok(self.and(other)))),
            "div" => Box::new(Dual::new(move |other: Number| {
                if other == other.class().zero() {
                    Err(TCError::unsupported("cannot divide by zero"))
                } else {
                    Ok(*self / other)
                }
            })),
            "mul" => Box::new(Dual::new(move |other| Ok(*self * other))),
            "sub" => Box::new(Dual::new(move |other| Ok(*self - other))),
            "pow" => Box::new(Dual::new(move |other| Ok(self.pow(other)))),

            // comparison
            "gt" => Box::new(Dual::new(move |other| Ok((*self > other).into()))),
            "gte" => Box::new(Dual::new(move |other| Ok((*self >= other).into()))),
            "lt" => Box::new(Dual::new(move |other| Ok((*self < other).into()))),
            "lte" => Box::new(Dual::new(move |other| Ok((*self <= other).into()))),
            "not" => Box::new(Unary::new("not", move || self.not())),
            "or" => Box::new(Dual::new(move |other| Ok(self.or(other)))),
            "xor" => Box::new(Dual::new(move |other| Ok(self.xor(other)))),

            // trigonometry
            "asin" => Box::new(Unary::new("abs", move || self.asin())),
            "sin" => Box::new(Unary::new("sin", move || self.sin())),
            "asinh" => Box::new(Unary::new("asinh", move || self.asinh())),
            "sinh" => Box::new(Unary::new("sinh", move || self.sinh())),

            "acos" => Box::new(Unary::new("acos", move || self.acos())),
            "cos" => Box::new(Unary::new("cos", move || self.cos())),
            "acosh" => Box::new(Unary::new("acosh", move || self.acosh())),
            "cosh" => Box::new(Unary::new("cosh", move || self.cosh())),

            "atan" => Box::new(Unary::new("atan", move || self.atan())),
            "tan" => Box::new(Unary::new("tan", move || self.tan())),
            "atanh" => Box::new(Unary::new("atanh", move || self.atanh())),
            "tanh" => Box::new(Unary::new("tanh", move || self.tanh())),

            _ => return None,
        };

        Some(handler)
    }
}

use std::fmt::Display;

use crate::vic::parse::{ArcSpan, RcVal, Rule};

/// Leverage `pest`'s pretty error reporting
pub fn expr_error(message: String, span: ArcSpan) -> String {
	let input = span.input();
	let span = pest::Span::new(&input, span.start(), span.end()).unwrap();
	expr_error2::<Rule>(message, span)
}
// Have to do this weird hack or else we have to attach '::<Rule>' to every error construction callsite
fn expr_error2<R: pest::RuleType>(message: String, span: pest::Span) -> String {
	pest::error::Error::new_from_span(pest::error::ErrorVariant::<R>::CustomError { message }, span).to_string()
}


/// `VicErr` is the error type used throughout the codebase.
///
/// It has two variants:
/// - `Full(span, message)`: an error with a specific span that caused the failure.
/// - `Simple(message)`: a general error message without source location info.
///
/// `Simple` is typically returned by lower-level operations that don't have access to span
/// information. As errors propagate upward, they can be "blamed" on a span using
/// [`Result<T, VicErr>::blame`] or [`try_blame`] to produce a `Full` error.
///
/// ### Example
/// ```rust
/// let span = ArcSpan::new();
///
/// let result1 = some_low_level_func(); // returns Result<T, VicErr>
/// let result2 = some_low_level_func().blame(span);
///
/// assert_eq!(result1, Err(VicErr::Simple("some error message".into())));
/// assert_eq!(result2, Err(VicErr::Full(span.clone(), "some error message".into())));
/// ```
///
/// This approach lets you defer attaching blame until you have the necessary context.
#[derive(Debug,Clone,PartialEq)]
pub enum VicErr {
	Full(ArcSpan,String),
	Simple(String),

	// These three are control flow that are returned as 'errors'
	// this pattern allows for signals to flow upwards easily through nested contexts
	Continue(ArcSpan),
	Break(ArcSpan),
	Return(ArcSpan,RcVal)
}

impl VicErr {
	pub fn simple(msg: String) -> Self {
		Self::Simple(msg)
	}
	pub fn full(span: ArcSpan, msg: String) -> Self {
		Self::Full(span, msg)
	}
	pub fn with_span(self, span: ArcSpan) -> Self {
		match self {
			VicErr::Full(_, msg) |
			VicErr::Simple(msg) => VicErr::Full(span, msg),
			_ => self
		}
	}
	pub fn try_with_span(self, span: ArcSpan) -> Self {
		match self {
			VicErr::Full(_, _) => self,
			VicErr::Simple(msg) => VicErr::Full(span, msg),
			_ => self
		}
	}
}

impl From<String> for VicErr {
	fn from(value: String) -> Self {
		Self::Simple(value)
	}
}

impl From<&str> for VicErr {
	fn from(value: &str) -> Self {
	  Self::Simple(value.to_string())
	}
}

impl Display for VicErr {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			VicErr::Full(arc_span, msg) => {
				let pretty_err = expr_error(msg.to_string(), arc_span.clone());
				write!(f, "{pretty_err}")
			}
			VicErr::Simple(msg) => {
				if msg.starts_with("vicut:") {
					write!(f,"{msg}")
				} else {
					write!(f,"vicut: {msg}")
				}
			}
			VicErr::Continue(arc_span) => {
				let pretty_err = expr_error("found 'continue' outside of loop context".into(), arc_span.clone());
				write!(f, "{pretty_err}")
			}
			VicErr::Break(arc_span) => {
				let pretty_err = expr_error("found 'break' outside of loop context".into(), arc_span.clone());
				write!(f, "{pretty_err}")
			}
			VicErr::Return(arc_span,_) => {
				let pretty_err = expr_error("found 'return' outside of function context".into(), arc_span.clone());
				write!(f, "{pretty_err}")
			}
		}
	}
}

pub trait VicErrResult<T> {
	fn blame(self,span: ArcSpan) -> Result<T,VicErr>;
	fn try_blame(self,span: ArcSpan) -> Result<T,VicErr>;
}

impl<T> VicErrResult<T> for Result<T,VicErr> {
	fn blame(self,span: ArcSpan) -> Result<T,VicErr> {
		self.map_err(|vic_err| vic_err.with_span(span))
	}
	fn try_blame(self,span: ArcSpan) -> Result<T,VicErr> {
		self.map_err(|vic_err| vic_err.try_with_span(span))
	}
}

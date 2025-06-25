#![allow(dead_code)]

//! Wait for the first of several futures to complete.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

// ====================================================================

/// Result for [`select5`].
#[derive(Debug, Clone)]
pub enum Either5<A, B, C, D, E> {
    /// First future finished first.
    First(A),
    /// Second future finished first.
    Second(B),
    /// Third future finished first.
    Third(C),
    /// Fourth future finished first.
    Fourth(D),
    /// Fifth future finished first.
    Fifth(E),
}

/// Same as [`select`], but with more futures.
pub fn select5<A, B, C, D, E>(a: A, b: B, c: C, d: D, e: E) -> Select5<A, B, C, D, E>
where
    A: Future,
    B: Future,
    C: Future,
    D: Future,
    E: Future,
{
    Select5 { a, b, c, d, e }
}

/// Future for the [`select5`] function.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Select5<A, B, C, D, E> {
    a: A,
    b: B,
    c: C,
    d: D,
    e: E,
}

impl<A, B, C, D, E> Future for Select5<A, B, C, D, E>
where
    A: Future,
    B: Future,
    C: Future,
    D: Future,
    E: Future,
{
    type Output = Either5<A::Output, B::Output, C::Output, D::Output, E::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        let a = unsafe { Pin::new_unchecked(&mut this.a) };
        let b = unsafe { Pin::new_unchecked(&mut this.b) };
        let c = unsafe { Pin::new_unchecked(&mut this.c) };
        let d = unsafe { Pin::new_unchecked(&mut this.d) };
        let e = unsafe { Pin::new_unchecked(&mut this.e) };
        if let Poll::Ready(x) = a.poll(cx) {
            return Poll::Ready(Either5::First(x));
        }
        if let Poll::Ready(x) = b.poll(cx) {
            return Poll::Ready(Either5::Second(x));
        }
        if let Poll::Ready(x) = c.poll(cx) {
            return Poll::Ready(Either5::Third(x));
        }
        if let Poll::Ready(x) = d.poll(cx) {
            return Poll::Ready(Either5::Fourth(x));
        }
        if let Poll::Ready(x) = e.poll(cx) {
            return Poll::Ready(Either5::Fifth(x));
        }
        Poll::Pending
    }
}

// ====================================================================

/// Result for [`select6`].
#[derive(Debug, Clone)]

pub enum Either6<A, B, C, D, E, F> {
    /// First future finished first.
    First(A),
    /// Second future finished first.
    Second(B),
    /// Third future finished first.
    Third(C),
    /// Fourth future finished first.
    Fourth(D),
    /// Fifth future finished first.
    Fifth(E),
    /// Sixth future finished first.
    Sixth(F),
}

/// Same as [`select`], but with more futures.
pub fn select6<A, B, C, D, E, F>(a: A, b: B, c: C, d: D, e: E, f: F) -> Select6<A, B, C, D, E, F>
where
    A: Future,
    B: Future,
    C: Future,
    D: Future,
    E: Future,
    F: Future,
{
    Select6 { a, b, c, d, e, f }
}

/// Future for the [`select6`] function.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Select6<A, B, C, D, E, F> {
    a: A,
    b: B,
    c: C,
    d: D,
    e: E,
    f: F,
}

impl<A, B, C, D, E, F> Future for Select6<A, B, C, D, E, F>
where
    A: Future,
    B: Future,
    C: Future,
    D: Future,
    E: Future,
    F: Future,
{
    type Output = Either6<A::Output, B::Output, C::Output, D::Output, E::Output, F::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        let a = unsafe { Pin::new_unchecked(&mut this.a) };
        let b = unsafe { Pin::new_unchecked(&mut this.b) };
        let c = unsafe { Pin::new_unchecked(&mut this.c) };
        let d = unsafe { Pin::new_unchecked(&mut this.d) };
        let e = unsafe { Pin::new_unchecked(&mut this.e) };
        let f = unsafe { Pin::new_unchecked(&mut this.f) };
        if let Poll::Ready(x) = a.poll(cx) {
            return Poll::Ready(Either6::First(x));
        }
        if let Poll::Ready(x) = b.poll(cx) {
            return Poll::Ready(Either6::Second(x));
        }
        if let Poll::Ready(x) = c.poll(cx) {
            return Poll::Ready(Either6::Third(x));
        }
        if let Poll::Ready(x) = d.poll(cx) {
            return Poll::Ready(Either6::Fourth(x));
        }
        if let Poll::Ready(x) = e.poll(cx) {
            return Poll::Ready(Either6::Fifth(x));
        }
        if let Poll::Ready(x) = f.poll(cx) {
            return Poll::Ready(Either6::Sixth(x));
        }
        Poll::Pending
    }
}

// ====================================================================

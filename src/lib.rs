use core::alloc::Layout;
use core::mem::ManuallyDrop;
use core::ptr::NonNull;
use core::{fmt, ops, ptr};

use std::process::abort;

extern crate alloc;

#[cfg(not(feature = "loom"))]
use {
    alloc::alloc::{alloc, dealloc},
    core::sync::atomic::{fence, AtomicUsize, Ordering},
};

#[cfg(feature = "loom")]
use loom::{
    alloc::{alloc, dealloc},
    sync::atomic::{fence, AtomicUsize, Ordering},
};

#[cfg(test)]
mod tests;

const MAX_REFCOUNT: usize = isize::MAX as usize;

pub struct Arc<T: ?Sized>(NonNull<ArcInner<T>>);

impl<T> Arc<T> {
    pub fn new(value: T) -> Self {
        let layout = Layout::new::<ArcInner<T>>();
        let ptr = unsafe { alloc(layout).cast::<ArcInner<T>>() };
        let Some(ptr) = NonNull::new(ptr) else {
            ::alloc::alloc::handle_alloc_error(layout);
        };
        unsafe { ptr::write(ptr.as_ptr(), ArcInner::new(value)) }
        Self(ptr)
    }
}

unsafe impl<T: Send + Sync + ?Sized> Send for Arc<T> {}
unsafe impl<T: Send + Sync + ?Sized> Sync for Arc<T> {}

impl<T: ?Sized> Arc<T> {
    pub fn downgrade(this: &Self) -> Weak<T> {
        unsafe { this.0.as_ref().acquire_weak_from_strong() }
        Weak(this.0)
    }
}

impl<T: ?Sized> ops::Deref for Arc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &self.0.as_ref().inner }
    }
}

impl<T: ?Sized> Drop for Arc<T> {
    fn drop(&mut self) {
        unsafe {
            ArcInner::release_strong(self.0);
        }
    }
}

impl<T: ?Sized> Clone for Arc<T> {
    fn clone(&self) -> Self {
        unsafe {
            self.0.as_ref().acquire_strong_from_strong();
        }
        Self(self.0)
    }
}

impl<T: fmt::Debug> fmt::Debug for Arc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = unsafe { self.0.as_ref() };
        f.debug_struct("Arc")
            .field("strong", &inner.strong.load(Ordering::Relaxed))
            .field("weak", &inner.weak.load(Ordering::Relaxed))
            .field("inner", &*inner.inner)
            .finish()
    }
}

pub struct Weak<T: ?Sized>(NonNull<ArcInner<T>>);

unsafe impl<T: Send + Sync + ?Sized> Send for Weak<T> {}
unsafe impl<T: Send + Sync + ?Sized> Sync for Weak<T> {}

impl<T> fmt::Debug for Weak<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Weak")
    }
}

const INVALID_WEAK_ADDR: usize = 1;

impl<T: ?Sized> Drop for Weak<T> {
    fn drop(&mut self) {
        if !self.is_dangling() {
            unsafe {
                ArcInner::release_weak(self.0);
            }
        }
    }
}

impl<T: ?Sized> Clone for Weak<T> {
    fn clone(&self) -> Self {
        if !self.is_dangling() {
            unsafe {
                self.0.as_ref().acquire_weak_from_weak();
            }
        }
        Self(self.0)
    }
}

impl<T> Weak<T> {
    pub const fn new() -> Self {
        let ptr = unsafe { NonNull::new_unchecked(INVALID_WEAK_ADDR as *mut _) };
        Self(ptr)
    }
}

impl<T: ?Sized> Weak<T> {
    fn is_dangling(&self) -> bool {
        self.0.as_ptr().cast::<u8>() as usize == INVALID_WEAK_ADDR
    }

    pub fn upgrade(&self) -> Option<Arc<T>> {
        if self.is_dangling() {
            return None;
        }
        if unsafe { self.0.as_ref().acquire_strong_from_weak() } {
            Some(Arc(self.0))
        } else {
            None
        }
    }
}

struct ArcInner<T: ?Sized> {
    strong: AtomicUsize,
    weak: AtomicUsize,
    inner: ManuallyDrop<T>,
}

const WEAK_EXIST: usize = 1;
const CLOSED: usize = 2;
const SINGLE_STRONG: usize = 4;
const SINGLE_WEAK: usize = 1;

impl<T> ArcInner<T> {
    fn new(inner: T) -> Self {
        Self {
            strong: SINGLE_STRONG.into(),
            weak: 0.into(),
            inner: ManuallyDrop::new(inner),
        }
    }
}

impl<T: ?Sized> ArcInner<T> {
    unsafe fn drop_inner(&mut self) {
        ManuallyDrop::drop(&mut self.inner);
    }

    unsafe fn dealloc(this: NonNull<Self>) {
        let layout = Layout::for_value(this.as_ref());
        dealloc(this.as_ptr().cast(), layout);
    }

    fn acquire_strong_from_strong(&self) {
        let old = self.strong.fetch_add(SINGLE_STRONG, Ordering::Relaxed);
        if old > MAX_REFCOUNT {
            abort();
        }
    }

    fn acquire_strong_from_weak(&self) -> bool {
        let old = self.strong.fetch_add(SINGLE_STRONG, Ordering::Acquire);
        if old > MAX_REFCOUNT {
            abort();
        }
        if old & CLOSED != 0 {
            return false;
        }
        if old < SINGLE_STRONG {
            debug_assert_eq!(old, WEAK_EXIST);
            let old_weak = self.weak.fetch_add(SINGLE_WEAK, Ordering::Relaxed);
            if old_weak > MAX_REFCOUNT {
                abort();
            }
        }
        true
    }

    unsafe fn release_strong(mut this: NonNull<Self>) {
        let this_ref = this.as_ref();
        let old = this_ref.strong.fetch_sub(SINGLE_STRONG, Ordering::Release);
        if old > SINGLE_STRONG + WEAK_EXIST {
            return;
        }
        if old & WEAK_EXIST == 0 {
            fence(Ordering::Acquire);
            this.as_mut().drop_inner();
            Self::dealloc(this);
            return;
        }
        if this_ref
            .strong
            .compare_exchange(WEAK_EXIST, CLOSED, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            this.as_mut().drop_inner();
        }
        Self::release_weak(this);
    }

    fn acquire_weak_from_strong(&self) {
        if self.weak.load(Ordering::Relaxed) == 0
            && self
                .weak
                .compare_exchange(0, SINGLE_WEAK * 2, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.strong.fetch_add(WEAK_EXIST, Ordering::Relaxed);
            return;
        }
        self.acquire_weak_from_weak();
    }

    fn acquire_weak_from_weak(&self) {
        let old = self.weak.fetch_add(SINGLE_WEAK, Ordering::Relaxed);
        if old > MAX_REFCOUNT {
            abort();
        }
    }

    unsafe fn release_weak(this: NonNull<Self>) {
        if this.as_ref().weak.fetch_sub(SINGLE_WEAK, Ordering::Relaxed) == SINGLE_WEAK {
            fence(Ordering::Acquire);
            Self::dealloc(this);
        }
    }
}

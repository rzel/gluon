use std::fmt;
use std::mem;
use std::ptr;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::ops::{Deref, DerefMut};
use std::cell::Cell;
use std::any::Any;
use std::marker::PhantomData;


#[inline]
unsafe fn allocate(size: usize) -> *mut u8 {
    // Allocate an extra element if it does not fit exactly
    let cap = size / mem::size_of::<f64>() +
              (if size % mem::size_of::<f64>() != 0 {
        1
    } else {
        0
    });
    ptr_from_vec(Vec::<f64>::with_capacity(cap))
}

#[inline]
fn ptr_from_vec(mut buf: Vec<f64>) -> *mut u8 {
    let ptr = buf.as_mut_ptr();
    mem::forget(buf);

    ptr as *mut u8
}

#[inline]
unsafe fn deallocate(ptr: *mut u8, old_size: usize) {
    let cap = old_size / mem::size_of::<f64>() +
              (if old_size % mem::size_of::<f64>() != 0 {
        1
    } else {
        0
    });
    Vec::<f64>::from_raw_parts(ptr as *mut f64, 0, cap);
}

pub struct WriteOnly<'s, T: ?Sized + 's>(*mut T, PhantomData<&'s mut T>);

impl<'s, T: ?Sized> WriteOnly<'s, T> {
    unsafe fn new(t: *mut T) -> WriteOnly<'s, T> {
        WriteOnly(t, PhantomData)
    }

    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.0
    }
}

impl<'s, T> WriteOnly<'s, T> {
    pub fn write(self, t: T) -> &'s mut T {
        unsafe {
            ptr::write(self.0, t);
            &mut *self.0
        }
    }
}

impl<'s, T: Copy> WriteOnly<'s, [T]> {
    pub fn write_slice(self, s: &[T]) -> &'s mut [T] {
        let self_ = unsafe { &mut *self.0 };
        assert!(s.len() == self_.len());
        for (to, from) in self_.iter_mut().zip(s) {
            *to = *from;
        }
        self_
    }
}

impl<'s> WriteOnly<'s, str> {
    pub fn write_str(self, s: &str) -> &'s mut str {
        unsafe {
            let ptr: &mut [u8] = mem::transmute::<*mut str, &mut [u8]>(self.0);
            assert!(s.len() == ptr.len());
            for (to, from) in ptr.iter_mut().zip(s.as_bytes()) {
                *to = *from;
            }
            &mut *self.0
        }
    }
}

#[derive(Debug)]
pub struct Error;

pub trait GcAllocator<T: ?Sized> {
    fn alloc<D>(&mut self, def: D) -> Result<GcPtr<D::Value>, Error>
        where D: DataDef<Value = T>,
              T: for<'a> FromPtr<&'a D>;
}

pub trait Gc {
    ///Unsafe since it calls collects if memory needs to be collected
    unsafe fn alloc_and_collect<R, D>(&mut self, roots: R, def: D) -> GcPtr<D::Value>
        where R: Traverseable<Self>,
              D: DataDef + Traverseable<Self>,
              Self: GcAllocator<D::Value>;

    ///Does a mark and sweep collection by walking from `roots`. This function is unsafe since
    ///roots need to cover all reachable object.
    unsafe fn collect<R>(&mut self, roots: R) where R: Traverseable<Self>;
}

#[derive(Debug)]
pub struct TypedGc<T: ?Sized> {
    values: Option<AllocPtr<T>>,
    allocated_memory: usize,
    collect_limit: usize,
}

pub unsafe trait FromPtr<D> {
    fn make_ptr(data: D, ptr: *mut ()) -> *mut Self;
}

unsafe impl<D, T> FromPtr<D> for T {
    fn make_ptr(_: D, ptr: *mut ()) -> *mut Self {
        ptr as *mut Self
    }
}

pub unsafe trait DataDef {
    type Value: ?Sized + for<'a> FromPtr<&'a Self>;
    fn size(&self) -> usize;
    fn initialize(self, ptr: WriteOnly<Self::Value>) -> &mut Self::Value;
}

///Datadefinition that moves its value directly into the pointer
///useful for sized types
pub struct Move<T>(pub T);

unsafe impl<T> DataDef for Move<T> {
    type Value = T;
    fn size(&self) -> usize {
        mem::size_of::<T>()
    }
    fn initialize(self, result: WriteOnly<T>) -> &mut T {
        result.write(self.0)
    }
}

impl<G, T> Traverseable<G> for Move<T> where T: Traverseable<G>
{
    fn traverse(&self, gc: &mut G) {
        self.0.traverse(gc);
    }
}

pub trait HasHeader {
    fn mark_header(&self) -> bool;
    fn align_of(&self) -> usize;
}

impl<T> HasHeader for T {
    fn mark_header(&self) -> bool {
        let header = unsafe {
            let p = self as *const T as *const u8;
            let header = p.offset(-(value_offset(self) as isize));
            &*(header as *const GcHeader<T>)
        };
        if header.header.marked.get() {
            true
        } else {
            header.header.marked.set(true);
            false
        }
    }

    fn align_of(&self) -> usize {
        mem::align_of::<T>()
    }
}

pub trait GcTraverseable<G: ?Sized>: Traverseable<G> + HasHeader { }

impl<G: ?Sized, T: ?Sized + Traverseable<G> + HasHeader> GcTraverseable<G> for T {}

#[derive(Debug)]
#[repr(C)]
struct Header<T: ?Sized> {
    next: Option<AllocPtr<T>>,
    value_size: usize,
    marked: Cell<bool>,
}

#[derive(Debug)]
#[repr(C)]
struct GcHeader<T: ?Sized> {
    header: Header<T>,
    value: T,
}


struct AllocPtr<T: ?Sized> {
    ptr: *mut GcHeader<T>,
}

impl<T> AllocPtr<T> {
    fn new(value_size: usize) -> AllocPtr<T> {
        unsafe {
            let alloc_size = sized_value_offset::<T>() + value_size;
            let ptr = &mut *(allocate(alloc_size) as *mut GcHeader<T>);
            ptr::write(&mut ptr.header,
                       Header {
                           next: None,
                           value_size: value_size,
                           marked: Cell::new(false),
                       });
            AllocPtr { ptr: ptr }
        }
    }
}

impl<T: ?Sized> AllocPtr<T> {
    fn size(&self) -> usize {
        value_offset(self) + self.header.value_size
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for AllocPtr<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f,
               "AllocPtr {{ ptr: {:?}, value: {:?} }}",
               &**self,
               &self.value)
    }
}

impl<T: ?Sized> Drop for AllocPtr<T> {
    fn drop(&mut self) {
        unsafe {
            let size = self.size();
            ptr::read(&self.header);
            // FIXME #18: drop the value as well
            deallocate(self.ptr as *mut u8, size);
        }
    }
}

impl<T: ?Sized> Deref for AllocPtr<T> {
    type Target = GcHeader<T>;
    fn deref(&self) -> &GcHeader<T> {
        unsafe { &*self.ptr }
    }
}

impl<T: ?Sized> DerefMut for AllocPtr<T> {
    fn deref_mut(&mut self) -> &mut GcHeader<T> {
        unsafe { &mut *self.ptr }
    }
}

impl<T: ?Sized> GcHeader<T> {
    fn value(&mut self) -> &mut T {
        &mut self.value
    }
}
fn value_offset<T: HasHeader>(value: &T) -> usize {
    let hs = mem::size_of::<Header<T>>();
    let max_align = value.align_of();
    hs + ((max_align - (hs % max_align)) % max_align)
}

fn sized_value_offset<T>() -> usize {
    let hs = mem::size_of::<Header<T>>();
    let max_align = mem::align_of::<T>();
    hs + ((max_align - (hs % max_align)) % max_align)
}


pub struct GcPtr<T: ?Sized> {
    ptr: *const T,
}

impl<T: ?Sized> Copy for GcPtr<T> {}

impl<T: ?Sized> Clone for GcPtr<T> {
    fn clone(&self) -> GcPtr<T> {
        GcPtr { ptr: self.ptr }
    }
}

impl<T: ?Sized> Deref for GcPtr<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.ptr }
    }
}

impl<T: ?Sized> ::std::borrow::Borrow<T> for GcPtr<T> {
    fn borrow(&self) -> &T {
        &**self
    }
}

impl<T: ?Sized + Eq> Eq for GcPtr<T> {}
impl<T: ?Sized + PartialEq> PartialEq for GcPtr<T> {
    fn eq(&self, other: &GcPtr<T>) -> bool {
        **self == **other
    }
}

impl<T: ?Sized + Ord> Ord for GcPtr<T> {
    fn cmp(&self, other: &GcPtr<T>) -> Ordering {
        (**self).cmp(&**other)
    }
}
impl<T: ?Sized + PartialOrd> PartialOrd for GcPtr<T> {
    fn partial_cmp(&self, other: &GcPtr<T>) -> Option<Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<T: ?Sized + Hash> Hash for GcPtr<T> {
    fn hash<H>(&self, state: &mut H)
        where H: Hasher
    {
        (**self).hash(state)
    }
}
impl<T: ?Sized + fmt::Debug> fmt::Debug for GcPtr<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "GcPtr({:?})", &**self)
    }
}
impl<T: ?Sized + fmt::Display> fmt::Display for GcPtr<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<'a, T> GcPtr<T> {
    fn header(&self) -> &GcHeader<T> {
        unsafe {
            let t: &T = self;
            let p = t as *const T as *const u8;
            let header = p.offset(-(value_offset(t) as isize));
            &*(header as *const GcHeader<T>)
        }
    }

    pub fn as_traverseable<G>(self) -> GcPtr<GcTraverseable<G> + 'a>
        where T: Traverseable<G> + 'a
    {
        GcPtr { ptr: self.ptr as *const GcTraverseable<G> }
    }
}

///Trait which must be implemented on all root types which contain GcPtr
///The type implementing Traverseable must call traverse on each of its fields
///which in turn contains GcPtr
pub trait Traverseable<G: ?Sized> {
    fn traverse(&self, gc: &mut G);
    ///Marks this object.
    ///Returns true if the pointer was already marked
    fn mark(&self, _gc: &mut G) -> bool {
        false
    }
}

impl<G, T> Traverseable<G> for Move<T> where T: Traverseable<G>
{
    fn traverse(&self, gc: &mut G) {
        self.0.traverse(gc)
    }
}

impl<G, T: ?Sized> Traverseable<G> for Box<T> where T: Traverseable<G>
{
    fn traverse(&self, gc: &mut G) {
        (**self).traverse(gc)
    }
}

impl<'a, G, T: ?Sized> Traverseable<G> for &'a T where T: Traverseable<G>
{
    fn traverse(&self, gc: &mut G) {
        (**self).traverse(gc);
    }
}

impl<'a, G, T: ?Sized> Traverseable<G> for &'a mut T where T: Traverseable<G>
{
    fn traverse(&self, gc: &mut G) {
        (**self).traverse(gc);
    }
}

macro_rules! tuple_traverse {
    () => {};
    ($first: ident $($id: ident)*) => {
        tuple_traverse!($($id)*);
        impl <Gc, $first $(,$id)*> Traverseable<Gc> for ($first, $($id,)*)
            where $first: Traverseable<Gc>
                  $(, $id: Traverseable<Gc>)* {
            #[allow(non_snake_case)]
            fn traverse(&self, gc: &mut Gc) {
                let (ref $first, $(ref $id,)*) = *self;
                $first.traverse(gc);
                $(
                    $id.traverse(gc);
                )*
            }
        }
    }
}

tuple_traverse!(A B C D E F G H I J);

impl<G> Traverseable<G> for () {
    fn traverse(&self, _: &mut G) {}
}

impl<G> Traverseable<G> for Any {
    fn traverse(&self, _: &mut G) {}
}

impl<G> Traverseable<G> for u8 {
    fn traverse(&self, _: &mut G) {}
}

impl<G> Traverseable<G> for str {
    fn traverse(&self, _: &mut G) {}
}

impl<G, T> Traverseable<G> for Cell<T> where T: Traverseable<G> + Copy
{
    fn traverse(&self, f: &mut G) {
        self.get().traverse(f);
    }
}

impl<G, U> Traverseable<G> for [U] where U: Traverseable<G>
{
    fn traverse(&self, f: &mut G) {
        for x in self.iter() {
            x.traverse(f);
        }
    }
}

impl<G, T> Traverseable<G> for Vec<T> where T: Traverseable<G>
{
    fn traverse(&self, gc: &mut G) {
        (**self).traverse(gc);
    }
}

///When traversing a GcPtr we need to mark it
impl<G, T: ?Sized + HasHeader> Traverseable<G> for GcPtr<T> where T: Traverseable<G>
{
    fn traverse(&self, gc: &mut G) {
        if !self.mark(gc) {
            // Continue traversing if this ptr was not already marked
            (**self).traverse(gc);
        }
    }

    fn mark(&self, _: &mut G) -> bool {
        T::mark_header(self)
    }
}

impl <T: Any, O: Any> GcAllocator<O> for TypedGc<T> {
    fn alloc<D>(&mut self, def: D) -> Result<GcPtr<D::Value>, Error>
        where D: DataDef<Value = O>,
              O: for<'a> FromPtr<&'a D>
    {
        use std::any::TypeId;
        if TypeId::of::<O>() == TypeId::of::<T>() {
            let ptr: GcPtr<T> = TypedGc::<T>::alloc(self, def);
            Ok(mem::transmute::<GcPtr<T>, GcPtr<D::Value>>(ptr))
        } else {
            Err(Error)
        }
    }
}

impl<T> TypedGc<T> {
    pub fn new() -> TypedGc<T> {
        TypedGc {
            values: None,
            allocated_memory: 0,
            collect_limit: 100,
        }
    }

    ///Unsafe since it calls collects if memory needs to be collected
    pub unsafe fn alloc_and_collect<R, D>(&mut self, roots: R, def: D) -> GcPtr<D::Value>
        where R: Traverseable<Self>,
              D: DataDef<Value = T> + Traverseable<Self>
    {
        if self.allocated_memory >= self.collect_limit {
            self.collect((roots, &def));
        }
        self.alloc(def)
    }

    pub fn alloc<D>(&mut self, def: D) -> GcPtr<D::Value>
        where D: DataDef<Value = T>
    {
        let size = def.size();
        let mut ptr = AllocPtr::new(size);
        ptr.header.next = self.values.take();
        self.allocated_memory += ptr.size();
        unsafe {
            let p: *mut D::Value = ptr.value();
            let ret: *const D::Value = &*def.initialize(WriteOnly::new(p));
            // Check that the returned pointer is the same as the one we sent as an extra precaution
            // that the pointer was initialized
            assert!(ret == p);
            self.values = Some(ptr);
            GcPtr { ptr: p }
        }
    }

    ///Does a mark and sweep collection by walking from `roots`. This function is unsafe since
    ///roots need to cover all reachable object.
    pub unsafe fn collect<R>(&mut self, roots: R)
        where R: Traverseable<Self>
    {
        debug!("Start collect");
        roots.traverse(self);
        self.sweep();
        self.collect_limit = 2 * self.allocated_memory;
    }


    pub fn object_count(&self) -> usize {
        let mut header: &GcHeader<T> = match self.values {
            Some(ref x) => &**x,
            None => return 0,
        };
        let mut count = 1;
        loop {
            match header.header.next {
                Some(ref ptr) => {
                    count += 1;
                    header = &**ptr;
                }
                None => break,
            }
        }
        count
    }

    pub unsafe fn sweep(&mut self) {
        // Usage of unsafe are sadly needed to circumvent the borrow checker
        let mut first = self.values.take();
        {
            let mut maybe_header = &mut first;
            loop {
                let current: &mut Option<AllocPtr<T>> = mem::transmute(&mut *maybe_header);
                maybe_header = match *maybe_header {
                    Some(ref mut header) => {
                        if !header.header.marked.get() {
                            let unreached = mem::replace(current, header.header.next.take());
                            self.free(unreached);
                            continue;
                        } else {
                            header.header.marked.set(false);
                            let next: &mut Option<AllocPtr<T>> = mem::transmute(&mut header.header
                                                                                           .next);
                            next
                        }
                    }
                    None => break,
                };
            }
        }
        self.values = first;
    }

    fn free(&mut self, header: Option<AllocPtr<T>>) {
        if let Some(ref ptr) = header {
            self.allocated_memory -= ptr.size();
        }
        drop(header);
    }
}


#[cfg(test)]
mod tests {
    use super::{TypedGc, GcPtr, GcHeader, Traverseable, DataDef, WriteOnly};
    use std::fmt;
    use std::mem;

    use self::Value::*;

    #[derive(Copy, Clone)]
    struct Data_ {
        fields: GcPtr<Vec<Value>>,
    }

    impl PartialEq for Data_ {
        fn eq(&self, other: &Data_) -> bool {
            self.fields.ptr == other.fields.ptr
        }
    }
    impl fmt::Debug for Data_ {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            self.fields.ptr.fmt(f)
        }
    }

    struct Def<'a> {
        elems: &'a [Value],
    }
    unsafe impl<'a> DataDef for Def<'a> {
        type Value = Vec<Value>;
        fn size(&self) -> usize {
            self.elems.len() * mem::size_of::<Value>()
        }
        fn initialize(self, result: WriteOnly<Vec<Value>>) -> &mut Vec<Value> {
            let vec = self.elems.iter().map(|x| *x).collect();
            result.write(vec)
        }
    }

    #[derive(Copy, Clone, PartialEq, Debug)]
    enum Value {
        Int(i32),
        Data(Data_),
    }

    impl<G> Traverseable<G> for Value {
        fn traverse(&self, gc: &mut G) {
            match *self {
                Data(ref data) => data.fields.traverse(gc),
                _ => (),
            }
        }
    }

    fn new_data(p: GcPtr<Vec<Value>>) -> Value {
        Data(Data_ { fields: p })
    }

    #[test]
    fn gc_header() {
        let mut gc = TypedGc::new();
        let ptr = gc.alloc(Def { elems: &[Int(1)] });
        let header: *const GcHeader<_> = ptr.header();
        let other: *const _ = &**gc.values.as_ref().unwrap();
        assert_eq!(header, other);
    }

    #[test]
    fn basic() {
        let mut gc = TypedGc::new();
        let mut stack: Vec<Value> = Vec::new();
        stack.push(new_data(gc.alloc(Def { elems: &[Int(1)] })));
        let d2 = new_data(gc.alloc(Def { elems: &[stack[0]] }));
        stack.push(d2);
        assert_eq!(gc.object_count(), 2);
        unsafe {
            gc.collect(&mut *stack);
        }
        assert_eq!(gc.object_count(), 2);
        match stack[0] {
            Data(ref data) => assert_eq!(data.fields[0], Int(1)),
            _ => panic!(),
        }
        match stack[1] {
            Data(ref data) => assert_eq!(data.fields[0], stack[0]),
            _ => panic!(),
        }
        stack.pop();
        stack.pop();
        unsafe {
            gc.collect(&mut *stack);
        }
        assert_eq!(gc.object_count(), 0);
    }
}

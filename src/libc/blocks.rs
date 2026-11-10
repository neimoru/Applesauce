/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `libBlocksRuntime` — Apple Blocks ABI helpers.
//!
//! These functions are called by the compiler-generated copy/dispose
//! helpers when an Objective-C block captures a `__strong` ObjC object,
//! a `__weak` reference, another block, or a `__block` storage variable.
//! They are documented in the
//! [Blocks ABI](https://clang.llvm.org/docs/Block-ABI-Apple.html#imported-variables-1).
//!
//! These implement the real semantics: stack blocks and `__block` variables
//! are copied to the heap and reference counted. Blocks are also Objective-C
//! objects (see `ns_block.rs`), so `-copy`/`-retain`/`-release` work too.

use crate::abi::{CallFromHost, GuestFunction};
use crate::dyld::{export_c_func, FunctionExports};
use crate::mem::{ConstPtr, ConstVoidPtr, MutPtr, MutVoidPtr, Ptr};
use crate::objc::{id, release, retain};
use crate::Environment;

/// Bit-flag values passed to `_Block_object_assign` / `_Block_object_dispose`.
/// See the Blocks ABI document referenced above.
const BLOCK_FIELD_IS_OBJECT: i32 = 3;
const BLOCK_FIELD_IS_BLOCK: i32 = 7;
const BLOCK_FIELD_IS_BYREF: i32 = 8;
const BLOCK_FIELD_IS_WEAK: i32 = 16;
const BLOCK_BYREF_CALLER: i32 = 128;

// Block literal layout: isa, flags, reserved, invoke, descriptor.
// Descriptor layout: reserved, size, copy helper, dispose helper.
const BLOCK_REFCOUNT_MASK: u32 = 0xfffe;
const BLOCK_NEEDS_FREE: u32 = 1 << 24;
const BLOCK_HAS_COPY_DISPOSE: u32 = 1 << 25;
const BLOCK_IS_GLOBAL: u32 = 1 << 28;
// `__block` variable (byref) layout: isa, forwarding, flags, size,
// [keep helper, destroy helper], variable.
const BLOCK_BYREF_HAS_COPY_DISPOSE: u32 = 1 << 25;

/// Is this block on the heap (made by [block_copy])?
fn is_heap_block(env: &Environment, block: ConstVoidPtr) -> bool {
    let flags: u32 = env.mem.read(block.cast::<u32>() + 1);
    flags & BLOCK_NEEDS_FREE != 0
}

/// `_Block_copy`: global blocks are returned as-is, heap blocks get their
/// reference count incremented, and stack blocks are copied to the heap.
pub fn block_copy(env: &mut Environment, block: ConstVoidPtr) -> ConstVoidPtr {
    if block.is_null() {
        return block;
    }
    let words: MutPtr<u32> = block.cast_mut().cast();
    let flags: u32 = env.mem.read(words + 1);
    if flags & BLOCK_IS_GLOBAL != 0 {
        return block;
    }
    if flags & BLOCK_NEEDS_FREE != 0 {
        if flags & BLOCK_REFCOUNT_MASK != BLOCK_REFCOUNT_MASK {
            env.mem.write(words + 1, flags + 2);
        }
        return block;
    }
    let descriptor: u32 = env.mem.read(words + 4);
    if descriptor == 0 {
        return block;
    }
    let descriptor: ConstPtr<u32> = Ptr::from_bits(descriptor);
    let size: u32 = env.mem.read(descriptor + 1);
    if !(20..0x10000).contains(&size) {
        log!("Warning: _Block_copy({:?}): implausible size {}, not copying", block, size);
        return block;
    }
    let copy: MutPtr<u32> = env.mem.alloc(size).cast();
    env.mem.memmove(copy.cast(), block, size);
    let malloc_class = env.objc.get_known_class("__NSMallocBlock__", &mut env.mem);
    env.mem.write(copy, malloc_class.to_bits());
    env.mem.write(copy + 1, (flags & !BLOCK_REFCOUNT_MASK) | BLOCK_NEEDS_FREE | 2);
    if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
        let copy_helper: u32 = env.mem.read(descriptor + 2);
        if copy_helper != 0 {
            let copy_helper = GuestFunction::from_addr_with_thumb_bit(copy_helper);
            let _: () = copy_helper.call_from_host(env, (copy.cast_const().cast::<std::ffi::c_void>(), block));
        }
    }
    copy.cast_const().cast()
}

/// `_Block_release`: decrement a heap block's reference count, freeing it
/// when it reaches zero. Stack and global blocks are unaffected.
pub fn block_release(env: &mut Environment, block: ConstVoidPtr) {
    if block.is_null() || !is_heap_block(env, block) {
        return;
    }
    let words: MutPtr<u32> = block.cast_mut().cast();
    let flags: u32 = env.mem.read(words + 1);
    let count = flags & BLOCK_REFCOUNT_MASK;
    if count == BLOCK_REFCOUNT_MASK {
        return; // saturated, leak
    }
    if count > 2 {
        env.mem.write(words + 1, flags - 2);
        return;
    }
    env.mem.write(words + 1, flags & !BLOCK_REFCOUNT_MASK);
    if flags & BLOCK_HAS_COPY_DISPOSE != 0 {
        let descriptor: ConstPtr<u32> = Ptr::from_bits(env.mem.read(words + 4));
        let dispose_helper: u32 = env.mem.read(descriptor + 3);
        if dispose_helper != 0 {
            let dispose_helper = GuestFunction::from_addr_with_thumb_bit(dispose_helper);
            let _: () = dispose_helper.call_from_host(env, (block,));
        }
    }
    env.mem.free(block.cast_mut());
}

/// Copy a `__block` variable to the heap (or retain the heap copy).
fn byref_copy(env: &mut Environment, src: ConstVoidPtr) -> ConstVoidPtr {
    let src_words: MutPtr<u32> = src.cast_mut().cast();
    let forwarding: MutPtr<u32> = Ptr::from_bits(env.mem.read(src_words + 1));
    if forwarding.is_null() {
        return src;
    }
    let fwd_flags: u32 = env.mem.read(forwarding + 2);
    if fwd_flags & BLOCK_NEEDS_FREE != 0 {
        // Already on the heap.
        if fwd_flags & BLOCK_REFCOUNT_MASK != BLOCK_REFCOUNT_MASK {
            env.mem.write(forwarding + 2, fwd_flags + 2);
        }
        return forwarding.cast_const().cast();
    }
    let size: u32 = env.mem.read(src_words + 3);
    if !(16..0x100000).contains(&size) {
        log!("Warning: byref copy of {:?}: implausible size {}, not copying", src, size);
        return src;
    }
    let copy: MutPtr<u32> = env.mem.alloc(size).cast();
    env.mem.memmove(copy.cast(), src, size);
    env.mem.write(copy + 1, copy.to_bits()); // copy->forwarding = copy
    env.mem.write(src_words + 1, copy.to_bits()); // src->forwarding = copy
    // One reference for the stack variable and one for the block.
    env.mem.write(copy + 2, (fwd_flags & !BLOCK_REFCOUNT_MASK) | BLOCK_NEEDS_FREE | 4);
    if fwd_flags & BLOCK_BYREF_HAS_COPY_DISPOSE != 0 {
        let keep: u32 = env.mem.read(src_words + 4);
        if keep != 0 {
            let keep = GuestFunction::from_addr_with_thumb_bit(keep);
            let _: () = keep.call_from_host(env, (copy.cast_const().cast::<std::ffi::c_void>(), src));
        }
    }
    copy.cast_const().cast()
}

fn byref_release(env: &mut Environment, byref: ConstVoidPtr) {
    let words: MutPtr<u32> = byref.cast_mut().cast();
    let forwarding: MutPtr<u32> = Ptr::from_bits(env.mem.read(words + 1));
    if forwarding.is_null() {
        return;
    }
    let flags: u32 = env.mem.read(forwarding + 2);
    if flags & BLOCK_NEEDS_FREE == 0 {
        return;
    }
    let count = flags & BLOCK_REFCOUNT_MASK;
    if count == BLOCK_REFCOUNT_MASK {
        return;
    }
    if count > 2 {
        env.mem.write(forwarding + 2, flags - 2);
        return;
    }
    // Last reference gone. The stack frame owning the original variable is
    // gone too by now, so this is safe to free.
    env.mem.write(forwarding + 2, flags & !BLOCK_REFCOUNT_MASK);
    if flags & BLOCK_BYREF_HAS_COPY_DISPOSE != 0 {
        let destroy: u32 = env.mem.read(forwarding + 5);
        if destroy != 0 {
            let destroy = GuestFunction::from_addr_with_thumb_bit(destroy);
            let _: () = destroy.call_from_host(env, (forwarding.cast_const().cast::<std::ffi::c_void>(),));
        }
    }
    env.mem.free(forwarding.cast());
}

fn _Block_copy(env: &mut Environment, block: ConstVoidPtr) -> ConstVoidPtr {
    block_copy(env, block)
}

fn _Block_release(env: &mut Environment, block: ConstVoidPtr) {
    block_release(env, block)
}

/// `_Block_object_assign(destAddr, object, flags)`. Called by the
/// compiler-generated copy helper to retain `object` and store it at
/// `destAddr`. We perform the retain side-effect via `objc::retain`.
///
/// `flags` is a bitwise OR of `BLOCK_FIELD_IS_*` constants telling us what
/// the captured value is — an ObjC object, another block, or a `__block`
/// storage location. For ObjC objects and blocks we retain; for `__weak`
/// captures we do nothing (per the Blocks ABI).
fn _Block_object_assign(
    env: &mut Environment,
    dest_addr: MutVoidPtr,
    object: ConstVoidPtr,
    flags: i32,
) {
    if flags & BLOCK_FIELD_IS_WEAK != 0 {
        // __weak: no retain. Just store the pointer.
        env.mem.write(dest_addr.cast(), object);
        return;
    }
    let kind = flags & 0xFF & !BLOCK_FIELD_IS_WEAK;
    let value = if kind & BLOCK_BYREF_CALLER != 0 {
        // Called from a byref keep helper: just retain/copy the variable.
        let kind = kind & !BLOCK_BYREF_CALLER;
        if kind == BLOCK_FIELD_IS_BLOCK {
            block_copy(env, object)
        } else {
            if kind == BLOCK_FIELD_IS_OBJECT {
                retain(env, Ptr::from_bits(object.to_bits()));
            }
            object
        }
    } else if kind == BLOCK_FIELD_IS_BYREF {
        byref_copy(env, object)
    } else if kind == BLOCK_FIELD_IS_BLOCK {
        block_copy(env, object)
    } else {
        if kind == BLOCK_FIELD_IS_OBJECT {
            let obj: id = Ptr::from_bits(object.to_bits());
            retain(env, obj);
        }
        object
    };
    env.mem.write(dest_addr.cast(), value);
}

/// `_Block_object_dispose(object, flags)`. Called by the compiler-generated
/// dispose helper to release a captured object that was retained by
/// `_Block_object_assign`.
fn _Block_object_dispose(env: &mut Environment, object: ConstVoidPtr, flags: i32) {
    if flags & BLOCK_FIELD_IS_WEAK != 0 {
        return;
    }
    let kind = flags & 0xFF & !BLOCK_FIELD_IS_WEAK & !BLOCK_BYREF_CALLER;
    if kind == BLOCK_FIELD_IS_BYREF {
        byref_release(env, object);
    } else if kind == BLOCK_FIELD_IS_BLOCK {
        block_release(env, object);
    } else if kind == BLOCK_FIELD_IS_OBJECT {
        let obj: id = Ptr::from_bits(object.to_bits());
        release(env, obj);
    }
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(_Block_copy(_)),
    export_c_func!(_Block_release(_)),
    export_c_func!(_Block_object_assign(_, _, _)),
    export_c_func!(_Block_object_dispose(_, _)),
];

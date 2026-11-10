/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Blocks as Objective-C objects.
//!
//! A block literal's `isa` is one of `_NSConcreteStackBlock`,
//! `_NSConcreteGlobalBlock` or `_NSConcreteMallocBlock`, which on iOS are the
//! class objects of `__NSStackBlock__`, `__NSGlobalBlock__` and
//! `__NSMallocBlock__`. dyld points those symbols at the classes below, so
//! that apps can send `-copy`, `-retain` etc. to blocks. Memory management
//! goes through the Blocks runtime (`src/libc/blocks.rs`).

use crate::libc::blocks::{block_copy, block_release};
use crate::mem::{ConstVoidPtr, MutVoidPtr};
use crate::objc::{id, objc_classes, ClassExports, NSZonePtr};
use super::NSUInteger;

fn as_block(this: id) -> ConstVoidPtr {
    this.cast_const().cast()
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSBlock: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    log!("Warning: attempt to allocate an NSBlock directly");
    crate::objc::nil
}

- (id)copy {
    block_copy(env, as_block(this)).cast_mut().cast()
}
- (id)copyWithZone:(NSZonePtr)_zone {
    block_copy(env, as_block(this)).cast_mut().cast()
}
- (id)retain {
    // Like on iOS, retaining a stack block does nothing; heap blocks are
    // reference counted.
    let _ = block_copy(env, as_block(this));
    this
}
- (())release {
    block_release(env, as_block(this))
}
- (id)autorelease {
    // Not tracked: autoreleasing a heap block leaks one reference.
    this
}
- (NSUInteger)retainCount {
    1
}
- (())dealloc {
}

- (())invoke {
    let invoke: u32 = env.mem.read(this.cast::<u32>() + 3);
    let invoke = crate::abi::GuestFunction::from_addr_with_thumb_bit(invoke);
    let _: () = crate::abi::CallFromHost::call_from_host(&invoke, env, (MutVoidPtr::from_bits(this.to_bits()),));
}

@end

@implementation __NSStackBlock__: NSBlock
- (id)retain {
    this
}
- (())release {
}
@end

@implementation __NSMallocBlock__: NSBlock
@end

@implementation __NSGlobalBlock__: NSBlock
- (id)copy {
    this
}
- (id)copyWithZone:(NSZonePtr)_zone {
    this
}
- (id)retain {
    this
}
- (())release {
}
@end

};

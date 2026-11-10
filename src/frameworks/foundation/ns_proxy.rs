/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSProxy`, the other root class.
//!
//! A proxy implements only memory management and a few basics itself;
//! messages it doesn't implement are sent through the message forwarding
//! machinery (`methodSignatureForSelector:` and `forwardInvocation:`, see
//! `objc_msgSend`), which subclasses override.

use super::NSUInteger;
use crate::objc::{
    id, msg, msg_class, msg_send_no_type_checking, nil, objc_classes, Class, ClassExports,
    NSZonePtr, ObjC, TrivialHostObject, SEL,
};
use crate::mem::MutVoidPtr;

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSProxy

+ (id)alloc {
    msg![env; this allocWithZone:(MutVoidPtr::null())]
}
+ (id)allocWithZone:(NSZonePtr)_zone {
    env.objc.alloc_object(this, Box::new(TrivialHostObject), &mut env.mem)
}

+ (Class)class {
    this
}

+ (())initialize {
}

+ (bool)respondsToSelector:(SEL)selector {
    let metaclass = ObjC::read_isa(this, &env.mem);
    env.objc.class_has_method(metaclass, selector)
}

+ (bool)instancesRespondToSelector:(SEL)selector {
    env.objc.class_has_method(this, selector)
}

+ (id)retain {
    this
}
+ (())release {
}
+ (id)autorelease {
    this
}

- (NSUInteger)retainCount {
    env.objc.get_refcount(this).into()
}
- (id)retain {
    env.objc.increment_refcount(this);
    this
}
- (())release {
    if env.objc.decrement_refcount(this) {
        () = msg![env; this dealloc];
    }
}
- (id)autorelease {
    () = msg_class![env; NSAutoreleasePool addObject:this];
    this
}
- (())dealloc {
    env.objc.dealloc_object(this, &mut env.mem)
}
- (())finalize {
}

- (Class)class {
    ObjC::read_isa(this, &env.mem)
}
- (Class)superclass {
    let class = ObjC::read_isa(this, &env.mem);
    env.objc.get_superclass(class)
}
- (id)self {
    this
}
- (bool)isProxy {
    true
}
- (NSZonePtr)zone {
    MutVoidPtr::null()
}
- (NSUInteger)hash {
    this.to_bits()
}
- (bool)isEqual:(id)other {
    this == other
}
- (id)description {
    let class = ObjC::read_isa(this, &env.mem);
    let desc = format!("<{}: {:?}>", env.objc.get_class_name(class), this);
    super::ns_string::from_rust_string(env, desc)
}
- (id)debugDescription {
    msg![env; this description]
}

- (bool)respondsToSelector:(SEL)selector {
    env.objc.object_has_method(&env.mem, this, selector)
}

- (id)performSelector:(SEL)sel {
    msg_send_no_type_checking(env, (this, sel))
}
- (id)performSelector:(SEL)sel withObject:(id)o1 {
    msg_send_no_type_checking(env, (this, sel, o1))
}
- (id)performSelector:(SEL)sel withObject:(id)o1 withObject:(id)o2 {
    msg_send_no_type_checking(env, (this, sel, o1, o2))
}

// The defaults: subclasses are expected to override these. On iOS these
// raise an exception; here we log and let the message return 0.
- (id)methodSignatureForSelector:(SEL)selector {
    log!(
        "Warning: -[NSProxy methodSignatureForSelector:{}] not overridden by {:?}",
        selector.as_str(&env.mem),
        this
    );
    nil
}
- (())forwardInvocation:(id)invocation {
    let sel: SEL = msg![env; invocation selector];
    log!(
        "Warning: -[NSProxy forwardInvocation:] for {} not handled by {:?}",
        sel.as_str(&env.mem),
        this
    );
}

@end

};

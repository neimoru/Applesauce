/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIPinchGestureRecognizer`.
//!
//! The base class `UIGestureRecognizer` is implemented in
//! `ui_gesture_recognizer.rs`. This module only provides the
//! `UIPinchGestureRecognizer` subclass with its `scale` and `velocity`
//! properties.
//!
//! Apple documentation:
//! - <https://developer.apple.com/documentation/uikit/uipinchgesturerecognizer>

use super::ui_gesture_recognizer::{GestureKind, UIGestureRecognizerHostObject};
use crate::frameworks::core_graphics::CGFloat;
use crate::objc::{id, objc_classes, ClassExports, NSZonePtr};

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIPinchGestureRecognizer: UIGestureRecognizer

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UIGestureRecognizerHostObject::new(GestureKind::Pinch));
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (CGFloat)scale {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).scale
}

- (())setScale:(CGFloat)scale {
    env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this).scale = scale;
}

- (CGFloat)velocity {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).scalar_velocity
}

@end

};

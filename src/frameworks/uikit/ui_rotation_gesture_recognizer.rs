/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIRotationGestureRecognizer`.
//!
//! Базовый класс `UIGestureRecognizer` уже определён в
//! `ui_pinch_gesture_recognizer.rs`, поэтому здесь объявлен только подкласс
//! `UIRotationGestureRecognizer`. Он наследует базовое состояние, обработку
//! `target/action` и т. п. от родителя.
//!
//! Как и в случае с `UIPinchGestureRecognizer`, диспетчеризация распознавания
//! на основе реальных касаний пока не реализована (см. комментарий в
//! `ui_view.rs`). Вместо этого мы предоставляем рабочий объект класса с
//! корректным поведением свойств `rotation` / `velocity`, чтобы приложения
//! могли создавать жест, устанавливать цели/действия, прикреплять его к
//! `UIView`, читать и записывать значения углов и не падали на отсутствующем
//! классе.
//!
//! Документация Apple:
//! - <https://developer.apple.com/documentation/uikit/uirotationgesturerecognizer>
//! - Свойство `rotation` хранится в радианах, относительно начала жеста.
//! - Свойство `velocity` (только для чтения) выражается в радианах в секунду.
//!
//! Эта реализация повторяет структуру `UIPinchGestureRecognizer`.

use super::ui_gesture_recognizer::{GestureKind, UIGestureRecognizerHostObject};
use crate::frameworks::core_graphics::CGFloat;
use crate::objc::{id, objc_classes, ClassExports, NSZonePtr};

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIRotationGestureRecognizer: UIGestureRecognizer

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UIGestureRecognizerHostObject::new(GestureKind::Rotation));
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (CGFloat)rotation {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).rotation
}

- (())setRotation:(CGFloat)rotation {
    env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this).rotation = rotation;
}

- (CGFloat)velocity {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).scalar_velocity
}

@end

};

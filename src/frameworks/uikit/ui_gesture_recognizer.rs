/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIGestureRecognizer` and the tap, swipe and pan recognizers.
//!
//! Touches delivered to a view are also sent to the gesture recognizers of
//! that view and its ancestors, as `touchesBegan:withEvent:` etc. messages,
//! so that apps' own subclasses (which override these and call `setState:`,
//! from `UIGestureRecognizerSubclass.h`) work. The base class implementations
//! of those methods do the recognition for the built-in recognizers.
//!
//! Resources:
//! - <https://developer.apple.com/documentation/uikit/uigesturerecognizer>
//! - <https://developer.apple.com/documentation/uikit/uipangesturerecognizer>

use crate::frameworks::core_graphics::{CGFloat, CGPoint};
use crate::frameworks::foundation::{NSInteger, NSUInteger};
use crate::objc::{id, msg, msg_send, nil, objc_classes, ClassExports, HostObject, NSZonePtr, SEL};
use crate::Environment;
use std::time::Instant;

pub type UIGestureRecognizerState = NSInteger;
pub const UIGestureRecognizerStatePossible: UIGestureRecognizerState = 0;
pub const UIGestureRecognizerStateBegan: UIGestureRecognizerState = 1;
pub const UIGestureRecognizerStateChanged: UIGestureRecognizerState = 2;
pub const UIGestureRecognizerStateEnded: UIGestureRecognizerState = 3;
pub const UIGestureRecognizerStateRecognized: UIGestureRecognizerState = 3;
pub const UIGestureRecognizerStateCancelled: UIGestureRecognizerState = 4;
pub const UIGestureRecognizerStateFailed: UIGestureRecognizerState = 5;

pub type UISwipeGestureRecognizerDirection = NSUInteger;
pub const UISwipeGestureRecognizerDirectionRight: UISwipeGestureRecognizerDirection = 1 << 0;
pub const UISwipeGestureRecognizerDirectionLeft: UISwipeGestureRecognizerDirection = 1 << 1;
pub const UISwipeGestureRecognizerDirectionUp: UISwipeGestureRecognizerDirection = 1 << 2;
pub const UISwipeGestureRecognizerDirectionDown: UISwipeGestureRecognizerDirection = 1 << 3;

/// How far (in points) a touch may move and still be a tap, and how far it
/// must move before a pan begins.
const MOVEMENT_THRESHOLD: CGFloat = 10.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum GestureKind {
    /// Plain `UIGestureRecognizer` or an app's own subclass of it: the
    /// subclass does the recognition.
    Generic,
    Tap,
    Swipe,
    Pan,
    Pinch,
    Rotation,
}

pub(super) struct UIGestureRecognizerHostObject {
    // UIKit does not retain targets, delegates or the associated view.
    target: id,
    action: Option<SEL>,
    delegate: id,
    pub(super) view: id,
    kind: GestureKind,
    state: UIGestureRecognizerState,
    enabled: bool,
    cancels_touches_in_view: bool,
    delays_touches_began: bool,
    delays_touches_ended: bool,
    number_of_taps_required: NSUInteger,
    number_of_touches_required: NSUInteger,
    minimum_number_of_touches: NSUInteger,
    maximum_number_of_touches: NSUInteger,
    direction: UISwipeGestureRecognizerDirection,
    /// Whether this recognizer is receiving the current touch sequence.
    tracking: bool,
    initial_location: CGPoint,
    current_location: CGPoint,
    /// Pan: the location at which the translation is zero.
    translation_origin: CGPoint,
    /// Pan: for computing the velocity.
    last_move: Option<(CGPoint, Instant)>,
    velocity: CGPoint,
    /// Pinch and rotation properties (no recognition is done for these).
    pub(super) scale: CGFloat,
    pub(super) rotation: CGFloat,
    pub(super) scalar_velocity: CGFloat,
}
impl HostObject for UIGestureRecognizerHostObject {}

impl UIGestureRecognizerHostObject {
    pub(super) fn new(kind: GestureKind) -> Self {
        let origin = CGPoint { x: 0.0, y: 0.0 };
        Self {
            target: nil,
            action: None,
            delegate: nil,
            view: nil,
            kind,
            state: UIGestureRecognizerStatePossible,
            enabled: true,
            cancels_touches_in_view: true,
            delays_touches_began: false,
            delays_touches_ended: true,
            number_of_taps_required: 1,
            number_of_touches_required: 1,
            minimum_number_of_touches: 1,
            maximum_number_of_touches: NSUInteger::MAX,
            direction: UISwipeGestureRecognizerDirectionRight,
            tracking: false,
            initial_location: origin,
            current_location: origin,
            translation_origin: origin,
            last_move: None,
            velocity: origin,
            scale: 1.0,
            rotation: 0.0,
            scalar_velocity: 0.0,
        }
    }
}

impl Default for UIGestureRecognizerHostObject {
    fn default() -> Self {
        Self::new(GestureKind::Generic)
    }
}

fn host(env: &Environment, recognizer: id) -> &UIGestureRecognizerHostObject {
    env.objc.borrow(recognizer)
}
fn host_mut(env: &mut Environment, recognizer: id) -> &mut UIGestureRecognizerHostObject {
    env.objc.borrow_mut(recognizer)
}

fn send_action(env: &mut Environment, recognizer: id) {
    let (target, action) = {
        let host = host(env, recognizer);
        (host.target, host.action)
    };
    let Some(action) = action else { return };
    if target == nil {
        log!("TODO: UIGestureRecognizer target:nil responder-chain dispatch is unsupported");
        return;
    }

    let colon_count = action
        .as_str(&env.mem)
        .bytes()
        .filter(|&b| b == b':')
        .count();
    match colon_count {
        0 => () = msg_send(env, (target, action)),
        1 => () = msg_send(env, (target, action, recognizer)),
        _ => log!(
            "Warning: unexpected gesture recognizer action {}, not sent",
            action.as_str(&env.mem)
        ),
    }
}

/// The location of (any) touch of `touches` in the recognizer's view.
fn touch_location(env: &mut Environment, recognizer: id, touches: id) -> Option<CGPoint> {
    let touch: id = msg![env; touches anyObject];
    if touch == nil {
        return None;
    }
    let view = host(env, recognizer).view;
    Some(msg![env; touch locationInView:view])
}

fn distance(a: CGPoint, b: CGPoint) -> CGFloat {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

fn set_state(env: &mut Environment, recognizer: id, new_state: UIGestureRecognizerState) {
    let old_state = host(env, recognizer).state;
    let starting = old_state == UIGestureRecognizerStatePossible
        && (new_state == UIGestureRecognizerStateBegan
            || new_state == UIGestureRecognizerStateRecognized);
    if starting && !delegate_allows_begin(env, recognizer) {
        host_mut(env, recognizer).state = UIGestureRecognizerStateFailed;
        return;
    }
    host_mut(env, recognizer).state = new_state;
    if matches!(
        new_state,
        UIGestureRecognizerStateBegan
            | UIGestureRecognizerStateChanged
            | UIGestureRecognizerStateEnded
            | UIGestureRecognizerStateCancelled
    ) {
        send_action(env, recognizer);
    }
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIGestureRecognizer: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UIGestureRecognizerHostObject::new(GestureKind::Generic));
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithTarget:(id)target action:(SEL)action {
    let recognizer = host_mut(env, this);
    recognizer.target = target;
    recognizer.action = if action.is_null() { None } else { Some(action) };
    this
}

- (())addTarget:(id)target action:(SEL)action {
    // Only one target-action pair is supported.
    let recognizer = host_mut(env, this);
    recognizer.target = target;
    recognizer.action = if action.is_null() { None } else { Some(action) };
}
- (())removeTarget:(id)target action:(SEL)_action {
    let recognizer = host_mut(env, this);
    if target == nil || recognizer.target == target {
        recognizer.target = nil;
        recognizer.action = None;
    }
}

- (id)delegate { host(env, this).delegate }
- (())setDelegate:(id)delegate {
    host_mut(env, this).delegate = delegate;
}
- (id)view { host(env, this).view }
- (())setView:(id)view {
    host_mut(env, this).view = view;
}
- (UIGestureRecognizerState)state {
    host(env, this).state
}
// From UIGestureRecognizerSubclass.h
- (())setState:(UIGestureRecognizerState)state {
    set_state(env, this, state);
}
- (())reset {
}
- (())ignoreTouch:(id)_touch forEvent:(id)_event {
}
- (())requireGestureRecognizerToFail:(id)_other {
}
- (bool)isEnabled { host(env, this).enabled }
- (())setEnabled:(bool)enabled {
    let host = host_mut(env, this);
    host.enabled = enabled;
    host.tracking = false;
    host.state = UIGestureRecognizerStatePossible;
}
- (bool)cancelsTouchesInView {
    host(env, this).cancels_touches_in_view
}
- (())setCancelsTouchesInView:(bool)value {
    host_mut(env, this).cancels_touches_in_view = value;
}
- (bool)delaysTouchesBegan {
    host(env, this).delays_touches_began
}
- (())setDelaysTouchesBegan:(bool)value {
    host_mut(env, this).delays_touches_began = value;
}
- (bool)delaysTouchesEnded {
    host(env, this).delays_touches_ended
}
- (())setDelaysTouchesEnded:(bool)value {
    host_mut(env, this).delays_touches_ended = value;
}
- (CGPoint)locationInView:(id)view {
    let (current_location, own_view) = {
        let host = host(env, this);
        (host.current_location, host.view)
    };
    if view == nil || view == own_view || own_view == nil {
        current_location
    } else {
        msg![env; view convertPoint:current_location fromView:own_view]
    }
}
- (NSUInteger)numberOfTouches {
    host(env, this).tracking as NSUInteger
}
- (CGPoint)locationOfTouch:(NSUInteger)_index inView:(id)view {
    msg![env; this locationInView:view]
}

// The base implementations do the recognition for the built-in recognizer
// types. Apps' subclasses override these.

- (())touchesBegan:(id)touches withEvent:(id)_event {
    let Some(location) = touch_location(env, this, touches) else { return };
    let host = host_mut(env, this);
    host.initial_location = location;
    host.current_location = location;
    host.translation_origin = location;
    host.last_move = Some((location, Instant::now()));
    host.velocity = CGPoint { x: 0.0, y: 0.0 };
}

- (())touchesMoved:(id)touches withEvent:(id)_event {
    let Some(location) = touch_location(env, this, touches) else { return };
    let (kind, state, initial) = {
        let host = host_mut(env, this);
        host.current_location = location;
        let now = Instant::now();
        if let Some((last, last_time)) = host.last_move {
            let dt = now.duration_since(last_time).as_secs_f32();
            if dt > 0.0 {
                // Smooth the velocity a little, like UIKit appears to.
                let v = CGPoint {
                    x: (location.x - last.x) / dt,
                    y: (location.y - last.y) / dt,
                };
                host.velocity = CGPoint {
                    x: host.velocity.x * 0.3 + v.x * 0.7,
                    y: host.velocity.y * 0.3 + v.y * 0.7,
                };
            }
        }
        host.last_move = Some((location, now));
        (host.kind, host.state, host.initial_location)
    };
    match kind {
        GestureKind::Tap => {
            if state == UIGestureRecognizerStatePossible
                && distance(location, initial) > MOVEMENT_THRESHOLD
            {
                set_state(env, this, UIGestureRecognizerStateFailed);
            }
        }
        GestureKind::Pan => {
            if state == UIGestureRecognizerStatePossible
                && distance(location, initial) >= MOVEMENT_THRESHOLD
            {
                set_state(env, this, UIGestureRecognizerStateBegan);
            } else if state == UIGestureRecognizerStateBegan
                || state == UIGestureRecognizerStateChanged
            {
                set_state(env, this, UIGestureRecognizerStateChanged);
            }
        }
        _ => {}
    }
}

- (())touchesEnded:(id)touches withEvent:(id)_event {
    let Some(location) = touch_location(env, this, touches) else { return };
    let (kind, state, initial, taps_required, direction, last_move) = {
        let host = host_mut(env, this);
        host.current_location = location;
        (
            host.kind,
            host.state,
            host.initial_location,
            host.number_of_taps_required,
            host.direction,
            host.last_move,
        )
    };
    let dx = location.x - initial.x;
    let dy = location.y - initial.y;
    match kind {
        GestureKind::Tap => {
            if state == UIGestureRecognizerStatePossible {
                if taps_required <= 1 && distance(location, initial) <= MOVEMENT_THRESHOLD {
                    set_state(env, this, UIGestureRecognizerStateRecognized);
                } else {
                    set_state(env, this, UIGestureRecognizerStateFailed);
                }
            }
        }
        GestureKind::Swipe => {
            let (swipe_direction, length, cross_length) = if dx.abs() >= dy.abs() {
                (
                    if dx >= 0.0 {
                        UISwipeGestureRecognizerDirectionRight
                    } else {
                        UISwipeGestureRecognizerDirectionLeft
                    },
                    dx.abs(),
                    dy.abs(),
                )
            } else {
                (
                    if dy >= 0.0 {
                        UISwipeGestureRecognizerDirectionDown
                    } else {
                        UISwipeGestureRecognizerDirectionUp
                    },
                    dy.abs(),
                    dx.abs(),
                )
            };
            if state == UIGestureRecognizerStatePossible {
                if length >= 24.0 && cross_length <= length && direction & swipe_direction != 0 {
                    set_state(env, this, UIGestureRecognizerStateRecognized);
                } else {
                    set_state(env, this, UIGestureRecognizerStateFailed);
                }
            }
        }
        GestureKind::Pan => {
            // A pan whose finger stopped before lifting has no velocity.
            if let Some((_, last_time)) = last_move {
                if last_time.elapsed().as_secs_f32() > 0.1 {
                    host_mut(env, this).velocity = CGPoint { x: 0.0, y: 0.0 };
                }
            }
            if state == UIGestureRecognizerStateBegan || state == UIGestureRecognizerStateChanged {
                set_state(env, this, UIGestureRecognizerStateEnded);
            } else if state == UIGestureRecognizerStatePossible {
                set_state(env, this, UIGestureRecognizerStateFailed);
            }
        }
        _ => {}
    }
}

- (())touchesCancelled:(id)_touches withEvent:(id)_event {
    let state = host(env, this).state;
    if state == UIGestureRecognizerStateBegan || state == UIGestureRecognizerStateChanged {
        set_state(env, this, UIGestureRecognizerStateCancelled);
    } else if state == UIGestureRecognizerStatePossible {
        set_state(env, this, UIGestureRecognizerStateFailed);
    }
}

@end

@implementation UITapGestureRecognizer: UIGestureRecognizer

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UIGestureRecognizerHostObject::new(GestureKind::Tap));
    env.objc.alloc_object(this, host_object, &mut env.mem)
}
- (NSUInteger)numberOfTapsRequired {
    host(env, this).number_of_taps_required
}
- (())setNumberOfTapsRequired:(NSUInteger)value {
    host_mut(env, this).number_of_taps_required = value;
}
- (NSUInteger)numberOfTouchesRequired {
    host(env, this).number_of_touches_required
}
- (())setNumberOfTouchesRequired:(NSUInteger)value {
    host_mut(env, this).number_of_touches_required = value;
}

@end

@implementation UISwipeGestureRecognizer: UIGestureRecognizer

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UIGestureRecognizerHostObject::new(GestureKind::Swipe));
    env.objc.alloc_object(this, host_object, &mut env.mem)
}
- (UISwipeGestureRecognizerDirection)direction {
    host(env, this).direction
}
- (())setDirection:(UISwipeGestureRecognizerDirection)value {
    host_mut(env, this).direction = value;
}
- (NSUInteger)numberOfTouchesRequired {
    host(env, this).number_of_touches_required
}
- (())setNumberOfTouchesRequired:(NSUInteger)value {
    host_mut(env, this).number_of_touches_required = value;
}

@end

@implementation UIPanGestureRecognizer: UIGestureRecognizer

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UIGestureRecognizerHostObject::new(GestureKind::Pan));
    env.objc.alloc_object(this, host_object, &mut env.mem)
}
- (NSUInteger)minimumNumberOfTouches {
    host(env, this).minimum_number_of_touches
}
- (())setMinimumNumberOfTouches:(NSUInteger)value {
    host_mut(env, this).minimum_number_of_touches = value;
}
- (NSUInteger)maximumNumberOfTouches {
    host(env, this).maximum_number_of_touches
}
- (())setMaximumNumberOfTouches:(NSUInteger)value {
    host_mut(env, this).maximum_number_of_touches = value;
}
- (CGPoint)translationInView:(id)_view {
    let host = host(env, this);
    CGPoint {
        x: host.current_location.x - host.translation_origin.x,
        y: host.current_location.y - host.translation_origin.y,
    }
}
- (())setTranslation:(CGPoint)translation inView:(id)_view {
    let host = host_mut(env, this);
    host.translation_origin = CGPoint {
        x: host.current_location.x - translation.x,
        y: host.current_location.y - translation.y,
    };
}
- (CGPoint)velocityInView:(id)_view {
    host(env, this).velocity
}

@end

};

pub(super) fn set_view(env: &mut Environment, recognizer: id, view: id) {
    host_mut(env, recognizer).view = view;
}

fn delegate_allows_touch(env: &mut Environment, recognizer: id, touch: id) -> bool {
    let delegate = host(env, recognizer).delegate;
    if delegate == nil {
        return true;
    }
    let Some(selector) = env
        .objc
        .lookup_selector("gestureRecognizer:shouldReceiveTouch:")
    else {
        return true;
    };
    if msg![env; delegate respondsToSelector:selector] {
        msg_send(env, (delegate, selector, recognizer, touch))
    } else {
        true
    }
}

fn delegate_allows_begin(env: &mut Environment, recognizer: id) -> bool {
    let delegate = host(env, recognizer).delegate;
    if delegate == nil {
        return true;
    }
    let Some(selector) = env.objc.lookup_selector("gestureRecognizerShouldBegin:") else {
        return true;
    };
    if msg![env; delegate respondsToSelector:selector] {
        msg_send(env, (delegate, selector, recognizer))
    } else {
        true
    }
}

/// The gesture recognizers that get the touches of a view: its own and its
/// ancestors'.
fn recognizers_for_view(env: &mut Environment, view: id) -> Vec<id> {
    let mut recognizers = Vec::new();
    let mut current = view;
    while current != nil {
        recognizers.extend(super::ui_view::gesture_recognizers(env, current));
        current = msg![env; current superview];
    }
    recognizers
}

pub(super) fn touches_began(env: &mut Environment, view: id, touches: id, event: id) {
    if view == nil {
        return;
    }
    let touch: id = msg![env; touches anyObject];
    if touch == nil {
        return;
    }
    for recognizer in recognizers_for_view(env, view) {
        if !host(env, recognizer).enabled || !delegate_allows_touch(env, recognizer, touch) {
            continue;
        }
        // A new touch sequence: a recognizer that finished the previous one
        // goes back to the "possible" state.
        let state = host(env, recognizer).state;
        if !host(env, recognizer).tracking
            && matches!(
                state,
                UIGestureRecognizerStateEnded
                    | UIGestureRecognizerStateCancelled
                    | UIGestureRecognizerStateFailed
            )
        {
            host_mut(env, recognizer).state = UIGestureRecognizerStatePossible;
            () = msg![env; recognizer reset];
        }
        host_mut(env, recognizer).tracking = true;
        () = msg![env; recognizer touchesBegan:touches withEvent:event];
    }
}

pub(super) fn touches_moved(env: &mut Environment, view: id, touches: id, event: id) {
    if view == nil {
        return;
    }
    for recognizer in recognizers_for_view(env, view) {
        if host(env, recognizer).tracking {
            () = msg![env; recognizer touchesMoved:touches withEvent:event];
        }
    }
}

pub(super) fn touches_ended(env: &mut Environment, view: id, touches: id, event: id) {
    if view == nil {
        return;
    }
    for recognizer in recognizers_for_view(env, view) {
        if host(env, recognizer).tracking {
            host_mut(env, recognizer).tracking = false;
            () = msg![env; recognizer touchesEnded:touches withEvent:event];
        }
    }
}

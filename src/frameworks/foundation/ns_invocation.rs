/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSInvocation` and `NSMethodSignature`.

use crate::abi::{extend_stack_for_args, write_next_arg};
use crate::cpu::Cpu;
use crate::frameworks::foundation::{NSInteger, NSUInteger};
use crate::libc::string::strdup;
use crate::mem::{ConstPtr, MutPtr, MutVoidPtr};
use crate::objc::{
    autorelease, id, msg, msg_class, objc_classes, objc_msgSend, release, retain,
    ClassExports, HostObject, NSZonePtr, SEL,
};

// =========================================================================
// MARK: - NSMethodSignature Host Object
// =========================================================================

#[derive(Default)]
struct NSMethodSignatureHostObject {
    return_type: String,
    argument_types: Vec<String>,
    // Кэшированные указатели на строки в памяти гостя для возврата через методы
    return_type_ptr: Option<MutPtr<u8>>,
    argument_type_ptrs: Vec<Option<MutPtr<u8>>>,
}
impl HostObject for NSMethodSignatureHostObject {}

// =========================================================================
// MARK: - NSInvocation Host Object
// =========================================================================

#[derive(Default)]
struct NSInvocationHostObject {
    /// `NSMethodSignature *`
    sig: id,
    /// Type encodings of the arguments (including self and _cmd).
    argument_types: Vec<String>,
    return_type: String,
    target: id,
    selector: Option<SEL>,
    /// The raw bytes of each argument (index 0 and 1 are unused: the target
    /// and selector are stored separately). `None` means not set yet.
    arguments: Vec<Option<Vec<u8>>>,
    /// The raw bytes of the return value (r0 and r1).
    return_value: [u32; 2],
    arguments_retained: bool,
    /// Objects retained by `retainArguments`.
    retained_objects: Vec<id>,
    /// C string copies made by `retainArguments`.
    copied_strings: Vec<MutPtr<u8>>,
}
impl HostObject for NSInvocationHostObject {}

/// Strip the type qualifiers (const, in, out, etc) from a type encoding.
fn strip_qualifiers(ty: &str) -> &str {
    ty.trim_start_matches(|c| "rnNoORVA".contains(c))
}

/// Parse one type from an Objective-C type encoding, starting at `*pos`.
/// Returns the (size, alignment) of the type, using the iOS ARMv7 rules
/// (8-byte types are only 4-byte aligned).
fn parse_type(b: &[u8], pos: &mut usize) -> (u32, u32) {
    while *pos < b.len() && b"rnNoORVA".contains(&b[*pos]) {
        *pos += 1;
    }
    if *pos >= b.len() {
        return (0, 1);
    }
    let c = b[*pos];
    *pos += 1;
    match c {
        b'v' => (0, 1),
        b'c' | b'C' | b'B' => (1, 1),
        b's' | b'S' => (2, 2),
        b'i' | b'I' | b'l' | b'L' | b'f' => (4, 4),
        b'q' | b'Q' | b'd' => (8, 4),
        b'#' | b':' | b'*' | b'?' => (4, 4),
        b'@' => {
            if *pos < b.len() && b[*pos] == b'?' {
                *pos += 1; // block
            } else if *pos < b.len() && b[*pos] == b'"' {
                *pos += 1;
                while *pos < b.len() && b[*pos] != b'"' {
                    *pos += 1;
                }
                *pos += 1;
            }
            (4, 4)
        }
        b'^' => {
            if *pos < b.len() {
                parse_type(b, pos);
            }
            (4, 4)
        }
        b'b' => {
            let mut bits = 0u32;
            while *pos < b.len() && b[*pos].is_ascii_digit() {
                bits = bits * 10 + (b[*pos] - b'0') as u32;
                *pos += 1;
            }
            (bits.div_ceil(8).max(1), 1)
        }
        b'[' => {
            let mut n = 0u32;
            while *pos < b.len() && b[*pos].is_ascii_digit() {
                n = n * 10 + (b[*pos] - b'0') as u32;
                *pos += 1;
            }
            let (size, align) = parse_type(b, pos);
            if *pos < b.len() && b[*pos] == b']' {
                *pos += 1;
            }
            (size * n, align)
        }
        b'{' | b'(' => {
            let close = if c == b'{' { b'}' } else { b')' };
            // Skip the name.
            while *pos < b.len() && b[*pos] != b'=' && b[*pos] != close {
                *pos += 1;
            }
            let mut size = 0u32;
            let mut align = 1u32;
            if *pos < b.len() && b[*pos] == b'=' {
                *pos += 1;
                while *pos < b.len() && b[*pos] != close {
                    if b[*pos] == b'"' {
                        // Field name.
                        *pos += 1;
                        while *pos < b.len() && b[*pos] != b'"' {
                            *pos += 1;
                        }
                        *pos += 1;
                        continue;
                    }
                    let (fsize, falign) = parse_type(b, pos);
                    align = align.max(falign);
                    if c == b'{' {
                        size = size.next_multiple_of(falign) + fsize;
                    } else {
                        size = size.max(fsize);
                    }
                }
            }
            *pos += 1; // closing bracket
            (size.next_multiple_of(align), align)
        }
        _ => (4, 4),
    }
}

/// Size in bytes of a single type encoding.
pub fn type_encoding_size(ty: &str) -> u32 {
    let mut pos = 0;
    parse_type(ty.as_bytes(), &mut pos).0
}

/// Split a method type encoding (e.g. `v12@0:4@8`) into its component types,
/// dropping the frame offsets.
pub fn split_type_encoding(types: &str) -> Vec<String> {
    let b = types.as_bytes();
    let mut res = Vec::new();
    let mut pos = 0;
    while pos < b.len() {
        if b[pos].is_ascii_digit() || b[pos] == b'-' || b[pos] == b'+' {
            pos += 1;
            continue;
        }
        let start = pos;
        parse_type(b, &mut pos);
        let pos_clamped = pos.min(b.len());
        res.push(String::from_utf8_lossy(&b[start..pos_clamped]).into_owned());
        pos = pos_clamped;
    }
    res
}

/// Number of 32-bit words an argument of this type occupies when passed.
fn arg_words(ty: &str) -> usize {
    (type_encoding_size(ty).max(1) as usize).div_ceil(4)
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// =========================================================================
// MARK: - NSMethodSignature
// =========================================================================

@implementation NSMethodSignature: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(NSMethodSignatureHostObject {
        return_type: String::from("v"),
        argument_types: vec![String::from("@"), String::from(":")], // self, _cmd
        return_type_ptr: None,
        argument_type_ptrs: vec![None, None],
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)signatureWithObjCTypes:(MutVoidPtr)_types {
    let sig: id = msg_class![env; NSMethodSignature alloc];
    let sig: id = msg![env; sig init];

    if !_types.is_null() {
        // ИСПРАВЛЕНИЕ E0308: Приводим Ptr<c_void> к Ptr<u8> через .cast()
        let types_str = env.mem.cstr_at_utf8(_types.cast_const().cast()).unwrap_or("");
        let mut parsed_types = split_type_encoding(types_str);

        if !parsed_types.is_empty() {
            let host = env.objc.borrow_mut::<NSMethodSignatureHostObject>(sig);
            host.return_type = parsed_types.remove(0);
            host.argument_type_ptrs = vec![None; parsed_types.len()];
            host.argument_types = parsed_types;
            host.return_type_ptr = None;
        }
    }

    autorelease(env, sig)
}

- (id)init {
    this
}

- (NSUInteger)numberOfArguments {
    env.objc.borrow::<NSMethodSignatureHostObject>(this).argument_types.len() as NSUInteger
}

- (())_touchHLE_setNumberOfArguments:(NSUInteger)count {
    let host = env.objc.borrow_mut::<NSMethodSignatureHostObject>(this);
    let count_usize = count as usize;
    host.argument_types.resize(count_usize, String::from("@"));
    host.argument_type_ptrs.resize(count_usize, None);
}

- (crate::mem::ConstPtr<std::ffi::c_char>)methodReturnType {
    let host = env.objc.borrow_mut::<NSMethodSignatureHostObject>(this);
    if host.return_type_ptr.is_none() {
        let bytes = host.return_type.as_bytes();
        let ptr: crate::mem::MutPtr<u8> = env.mem.alloc(bytes.len() as u32 + 1).cast();
        for (i, &b) in bytes.iter().enumerate() {
            // ИСПРАВЛЕНИЕ E0599: Используем оператор сложения (ptr + i) вместо
            // .offset()
            env.mem.write(ptr + (i as u32), b);
        }
        env.mem.write(ptr + (bytes.len() as u32), 0u8);
        host.return_type_ptr = Some(ptr);
    }
    host.return_type_ptr.unwrap().cast_const().cast()
}

- (crate::mem::ConstPtr<std::ffi::c_char>)getArgumentTypeAtIndex:(NSUInteger)_index {
    let host = env.objc.borrow_mut::<NSMethodSignatureHostObject>(this);
    let idx = _index as usize;

    if idx < host.argument_types.len() {
        if host.argument_type_ptrs[idx].is_none() {
            let bytes = host.argument_types[idx].as_bytes();
            let ptr: crate::mem::MutPtr<u8> = env.mem.alloc(bytes.len() as u32 + 1).cast();
            for (i, &b) in bytes.iter().enumerate() {
                // ИСПРАВЛЕНИЕ E0599: Используем ptr + i
                env.mem.write(ptr + (i as u32), b);
            }
            env.mem.write(ptr + (bytes.len() as u32), 0u8);
            host.argument_type_ptrs[idx] = Some(ptr);
        }
        host.argument_type_ptrs[idx].unwrap().cast_const().cast()
    } else {
        // Fallback на случай выхода за границы
        let ptr: crate::mem::MutPtr<u8> = env.mem.alloc(2).cast();
        // ИСПРАВЛЕНИЕ E0599: Используем ptr и ptr + 1
        env.mem.write(ptr, b'@');
        env.mem.write(ptr + 1u32, 0);
        ptr.cast_const().cast()
    }
}

- (NSUInteger)methodReturnLength {
    let ret_type_ptr: crate::mem::ConstPtr<std::ffi::c_char> = msg![env; this methodReturnType];
    if ret_type_ptr.is_null() {
        return 0;
    }

    let ret_type_str = env.mem.cstr_at_utf8(ret_type_ptr.cast()).unwrap_or("");
    type_encoding_size(ret_type_str)
}

- (NSUInteger)frameLength {
    let host = env.objc.borrow::<NSMethodSignatureHostObject>(this);
    host.argument_types.iter().map(|t| arg_words(t) as NSUInteger * 4).sum()
}

- (bool)isOneway {
    env.objc.borrow::<NSMethodSignatureHostObject>(this).return_type.starts_with('V')
}

- (())dealloc {
    {
        let host = env.objc.borrow::<NSMethodSignatureHostObject>(this);
        if let Some(ptr) = host.return_type_ptr {
            env.mem.free(ptr.cast());
        }
        for ptr in host.argument_type_ptrs.iter().flatten() {
            env.mem.free(ptr.cast());
        }
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

// =========================================================================
// MARK: - NSInvocation
// =========================================================================

@implementation NSInvocation: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<NSInvocationHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)invocationWithMethodSignature:(id)sig {
    retain(env, sig);
    let num_of_args: NSUInteger = msg![env; sig numberOfArguments];
    let mut argument_types: Vec<String> = Vec::with_capacity(num_of_args as usize);

    for i in 0..num_of_args {
        let type_ptr: ConstPtr<u8> = msg![env; sig getArgumentTypeAtIndex:i];
        argument_types.push(env.mem.cstr_at_utf8(type_ptr).unwrap_or("@").to_string());
    }
    let ret_ptr: ConstPtr<u8> = msg![env; sig methodReturnType];
    let return_type = if ret_ptr.is_null() {
        String::from("v")
    } else {
        env.mem.cstr_at_utf8(ret_ptr).unwrap_or("v").to_string()
    };

    let host_object = Box::new(NSInvocationHostObject {
        sig,
        arguments: vec![None; num_of_args as usize],
        argument_types,
        return_type,
        ..Default::default()
    });
    let res = env.objc.alloc_object(this, host_object, &mut env.mem);
    autorelease(env, res)
}

- (id)init {
    this
}

- (id)methodSignature {
    env.objc.borrow::<NSInvocationHostObject>(this).sig
}

- (id)target {
    env.objc.borrow::<NSInvocationHostObject>(this).target
}

- (())setTarget:(id)target {
    let old_target = env.objc.borrow::<NSInvocationHostObject>(this).target;
    let arguments_retained = env.objc.borrow::<NSInvocationHostObject>(this).arguments_retained;
    env.objc.borrow_mut::<NSInvocationHostObject>(this).target = target;
    if arguments_retained {
        retain(env, target);
        release(env, old_target);
    }
}

- (SEL)selector {
    env.objc.borrow::<NSInvocationHostObject>(this).selector.unwrap_or(SEL::null())
}

- (())setSelector:(SEL)selector {
    env.objc.borrow_mut::<NSInvocationHostObject>(this).selector = Some(selector);
}

- (bool)argumentsRetained {
    env.objc.borrow::<NSInvocationHostObject>(this).arguments_retained
}

- (())retainArguments {
    if env.objc.borrow::<NSInvocationHostObject>(this).arguments_retained {
        return;
    }

    let target = env.objc.borrow::<NSInvocationHostObject>(this).target;
    retain(env, target);

    let mut retained_objects: Vec<id> = Vec::new();
    let mut copied_strings: Vec<MutPtr<u8>> = Vec::new();

    let num_of_args = env.objc.borrow::<NSInvocationHostObject>(this).argument_types.len();
    for i in 2..num_of_args {
        let host = env.objc.borrow::<NSInvocationHostObject>(this);
        let Some(bytes) = host.arguments[i].as_ref() else { continue };
        let word = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        match strip_qualifiers(&host.argument_types[i]) {
            ty if ty.starts_with('@') => {
                let obj: id = crate::mem::Ptr::from_bits(word);
                if ty == "@?" {
                    // Blocks need to be copied to the heap, not retained.
                    // (The copy is leaked, which is harmless.)
                    if !obj.is_null() {
                        let copy = crate::libc::dispatch::copy_block_to_heap(env, obj.cast());
                        env.objc.borrow_mut::<NSInvocationHostObject>(this).arguments[i]
                            .as_mut().unwrap()[..4].copy_from_slice(&copy.to_bits().to_le_bytes());
                    }
                } else {
                    retain(env, obj);
                    retained_objects.push(obj);
                }
            }
            "*" => {
                let str: ConstPtr<u8> = crate::mem::Ptr::from_bits(word);
                if !str.is_null() {
                    let str_copy = strdup(env, str);
                    env.objc.borrow_mut::<NSInvocationHostObject>(this).arguments[i]
                        .as_mut().unwrap()[..4].copy_from_slice(&str_copy.to_bits().to_le_bytes());
                    copied_strings.push(str_copy);
                }
            }
            _ => {}
        }
    }

    let host = env.objc.borrow_mut::<NSInvocationHostObject>(this);
    host.retained_objects = retained_objects;
    host.copied_strings = copied_strings;
    host.arguments_retained = true;
}

- (())getArgument:(MutVoidPtr)buffer atIndex:(NSInteger)index {
    if index == 0 {
        let target = env.objc.borrow::<NSInvocationHostObject>(this).target;
        env.mem.write(buffer.cast(), target);
        return;
    }
    if index == 1 {
        let sel = env.objc.borrow::<NSInvocationHostObject>(this).selector.unwrap_or(SEL::null());
        env.mem.write(buffer.cast(), sel);
        return;
    }
    let host = env.objc.borrow::<NSInvocationHostObject>(this);
    let Some(ty) = host.argument_types.get(index as usize) else {
        log!("Warning: -[NSInvocation getArgument:atIndex:{}] out of range", index);
        return;
    };
    let size = type_encoding_size(ty) as usize;
    let bytes = host.arguments[index as usize].clone().unwrap_or_else(|| vec![0; size.max(4)]);
    if size > 0 {
        env.mem.bytes_at_mut(buffer.cast(), size as u32).copy_from_slice(&bytes[..size]);
    }
}

- (())setArgument:(MutVoidPtr)arg_loc atIndex:(NSInteger)idx {
    if idx == 0 {
        let target: id = env.mem.read(arg_loc.cast());
        () = msg![env; this setTarget:target];
        return;
    }
    if idx == 1 {
        let sel: SEL = env.mem.read(arg_loc.cast());
        env.objc.borrow_mut::<NSInvocationHostObject>(this).selector = Some(sel);
        return;
    }
    let (arguments_retained, ty) = {
        let host = env.objc.borrow::<NSInvocationHostObject>(this);
        let Some(ty) = host.argument_types.get(idx as usize) else {
            log!("Warning: -[NSInvocation setArgument:atIndex:{}] out of range", idx);
            return;
        };
        (host.arguments_retained, strip_qualifiers(ty).to_string())
    };
    let size = type_encoding_size(&ty) as usize;
    // Always keep at least a whole number of words.
    let mut bytes = vec![0u8; (arg_words(&ty) * 4).max(4)];
    if size > 0 {
        bytes[..size].copy_from_slice(env.mem.bytes_at(arg_loc.cast(), size as u32));
    }

    if arguments_retained {
        let new_word = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        let old_word = env.objc.borrow::<NSInvocationHostObject>(this).arguments[idx as usize]
            .as_ref()
            .map(|b| u32::from_le_bytes(b[..4].try_into().unwrap()));
        if ty.starts_with('@') {
            let new_obj: id = crate::mem::Ptr::from_bits(new_word);
            retain(env, new_obj);
            env.objc.borrow_mut::<NSInvocationHostObject>(this).retained_objects.push(new_obj);
            if let Some(old_word) = old_word {
                let old_obj: id = crate::mem::Ptr::from_bits(old_word);
                let host = env.objc.borrow_mut::<NSInvocationHostObject>(this);
                if let Some(pos) = host.retained_objects.iter().position(|&o| o == old_obj) {
                    host.retained_objects.swap_remove(pos);
                    release(env, old_obj);
                }
            }
        } else if ty == "*" {
            let new_str: ConstPtr<u8> = crate::mem::Ptr::from_bits(new_word);
            if !new_str.is_null() {
                let copy = strdup(env, new_str);
                bytes[..4].copy_from_slice(&copy.to_bits().to_le_bytes());
                env.objc.borrow_mut::<NSInvocationHostObject>(this).copied_strings.push(copy);
            }
            if let Some(old_word) = old_word {
                let old_str: MutPtr<u8> = crate::mem::Ptr::from_bits(old_word);
                let host = env.objc.borrow_mut::<NSInvocationHostObject>(this);
                if let Some(pos) = host.copied_strings.iter().position(|&o| o == old_str) {
                    host.copied_strings.swap_remove(pos);
                    env.mem.free(old_str.cast());
                }
            }
        }
    }

    env.objc.borrow_mut::<NSInvocationHostObject>(this).arguments[idx as usize] = Some(bytes);
}

- (())getReturnValue:(MutVoidPtr)buffer {
    let host = env.objc.borrow::<NSInvocationHostObject>(this);
    let size = type_encoding_size(&host.return_type).min(8);
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&host.return_value[0].to_le_bytes());
    bytes[4..].copy_from_slice(&host.return_value[1].to_le_bytes());
    if size > 0 {
        env.mem.bytes_at_mut(buffer.cast(), size).copy_from_slice(&bytes[..size as usize]);
    }
}

- (())setReturnValue:(MutVoidPtr)buffer {
    let size = {
        let host = env.objc.borrow::<NSInvocationHostObject>(this);
        type_encoding_size(&host.return_type).min(8)
    };
    let mut bytes = [0u8; 8];
    if size > 0 {
        bytes[..size as usize].copy_from_slice(env.mem.bytes_at(buffer.cast(), size));
    }
    env.objc.borrow_mut::<NSInvocationHostObject>(this).return_value = [
        u32::from_le_bytes(bytes[..4].try_into().unwrap()),
        u32::from_le_bytes(bytes[4..].try_into().unwrap()),
    ];
}

- (())_touchHLE_getReturnRegisters:(MutPtr<u32>)out {
    let rv = env.objc.borrow::<NSInvocationHostObject>(this).return_value;
    env.mem.write(out, rv[0]);
    env.mem.write(out + 1, rv[1]);
}

- (())invokeWithTarget:(id)target {
    () = msg![env; this setTarget:target];
    () = msg![env; this invoke];
}

- (())invoke {
    let Some(selector) = env.objc.borrow::<NSInvocationHostObject>(this).selector else {
        log!("Warning: NSInvocation invoked without a selector!");
        return;
    };
    let host = env.objc.borrow::<NSInvocationHostObject>(this);
    if type_encoding_size(&host.return_type) > 8 {
        log!(
            "Warning: NSInvocation with struct return type {} is not supported (selector {})",
            host.return_type,
            selector.as_str(&env.mem)
        );
    }
    let target = host.target;

    // Flatten the arguments into a list of words, as they would be passed.
    let mut words: Vec<u32> = vec![target.to_bits(), selector.to_bits()];
    for i in 2..host.argument_types.len() {
        let n = arg_words(&host.argument_types[i]);
        match host.arguments[i].as_ref() {
            Some(bytes) => {
                for w in 0..n {
                    words.push(u32::from_le_bytes(bytes[w * 4..w * 4 + 4].try_into().unwrap()));
                }
            }
            None => {
                log!("Warning: NSInvocation invoked without argument {} set", i);
                words.extend(std::iter::repeat_n(0, n));
            }
        }
    }

    let regs = env.cpu.regs_mut();
    let old_sp = extend_stack_for_args(words.len(), regs);
    let mut reg_offset = 0;
    for word in words {
        let regs = env.cpu.regs_mut();
        write_next_arg::<u32>(&mut reg_offset, regs, &mut env.mem, word);
    }

    objc_msgSend(env, target, selector);

    let regs = env.cpu.regs_mut();
    let rv = [regs[0], regs[1]];
    regs[Cpu::SP] = old_sp;
    env.objc.borrow_mut::<NSInvocationHostObject>(this).return_value = rv;
}

- (())dealloc {
    let &NSInvocationHostObject { sig, target, arguments_retained, .. } = env.objc.borrow::<NSInvocationHostObject>(this);
    release(env, sig);

    if arguments_retained {
        release(env, target);
        let retained_objects = std::mem::take(
            &mut env.objc.borrow_mut::<NSInvocationHostObject>(this).retained_objects
        );
        for obj in retained_objects {
            release(env, obj);
        }
        let copied_strings = std::mem::take(
            &mut env.objc.borrow_mut::<NSInvocationHostObject>(this).copied_strings
        );
        for s in copied_strings {
            env.mem.free(s.cast());
        }
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

};

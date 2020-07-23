use std::cmp::Ordering;
use std::convert::TryFrom;
use std::fmt;
use std::ops::{Add, Mul};

use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::error;

use super::class::{CastFrom, CastInto, Impl, NumberClass, NumberImpl, ValueImpl};
use super::class::{ComplexType, FloatType, IntType, NumberType, UIntType};
use super::TCResult;

#[derive(Clone, PartialEq)]
pub enum Complex {
    C32(num::Complex<f32>),
    C64(num::Complex<f64>),
}

impl Impl for Complex {
    type Class = ComplexType;

    fn class(&self) -> ComplexType {
        match self {
            Complex::C32(_) => ComplexType::C32,
            Complex::C64(_) => ComplexType::C64,
        }
    }
}

impl ValueImpl for Complex {
    type Class = ComplexType;
}

impl NumberImpl for Complex {
    type Abs = Float;
    type Class = ComplexType;

    fn abs(&self) -> Float {
        match self {
            Self::C32(c) => Float::F32(c.norm_sqr()),
            Self::C64(c) => Float::F64(c.norm_sqr()),
        }
    }
}

impl CastFrom<Number> for Complex {
    fn cast_from(number: Number) -> Complex {
        use Number::*;
        match number {
            Bool(b) => b.cast_into(),
            Complex(c) => c,
            Float(f) => Self::cast_from(f),
            Int(i) => Self::cast_from(i),
            UInt(u) => Self::cast_from(u),
        }
    }
}

impl CastFrom<Complex> for bool {
    fn cast_from(c: Complex) -> bool {
        use Complex::*;
        match c {
            C32(c) if c.norm_sqr() == 0f32 => false,
            C64(c) if c.norm_sqr() == 0f64 => false,
            _ => true,
        }
    }
}

impl Eq for Complex {}

impl Add for Complex {
    type Output = Self;

    fn add(self, other: Complex) -> Self {
        match (self, other) {
            (Self::C32(l), Self::C32(r)) => Self::C32(l + r),
            (Self::C64(l), Self::C64(r)) => Self::C64(l + r),
            (Self::C64(l), r) => {
                let r: num::Complex<f64> = r.into();
                Self::C64(l + r)
            }
            (l, r) => r + l,
        }
    }
}

impl Mul for Complex {
    type Output = Self;

    fn mul(self, other: Complex) -> Self {
        match (self, other) {
            (Self::C32(l), Self::C32(r)) => Self::C32(l * r),
            (Self::C64(l), Self::C64(r)) => Self::C64(l * r),
            (Self::C64(l), r) => {
                let r: num::Complex<f64> = r.into();
                Self::C64(l * r)
            }
            (l, r) => r * l,
        }
    }
}

impl PartialOrd for Complex {
    fn partial_cmp(&self, other: &Complex) -> Option<Ordering> {
        match (self, other) {
            (Complex::C32(l), Complex::C32(r)) => l.norm_sqr().partial_cmp(&r.norm_sqr()),
            (Complex::C64(l), Complex::C64(r)) => l.norm_sqr().partial_cmp(&r.norm_sqr()),
            _ => None,
        }
    }
}

impl From<Complex> for num::Complex<f64> {
    fn from(c: Complex) -> Self {
        match c {
            Complex::C32(c) => num::Complex::new(c.re as f64, c.im as f64),
            Complex::C64(c64) => c64,
        }
    }
}

impl From<Float> for Complex {
    fn from(f: Float) -> Self {
        match f {
            Float::F64(f) => Self::C64(num::Complex::new(f, 0.0f64)),
            Float::F32(f) => Self::C32(num::Complex::new(f, 0.0f32)),
        }
    }
}

impl CastFrom<Float> for Complex {
    fn cast_from(f: Float) -> Self {
        f.into()
    }
}

impl From<Int> for Complex {
    fn from(i: Int) -> Self {
        match i {
            Int::I64(i) => Self::C64(num::Complex::new(i as f64, 0.0f64)),
            Int::I32(i) => Self::C32(num::Complex::new(i as f32, 0.0f32)),
            Int::I16(i) => Self::C32(num::Complex::new(i as f32, 0.0f32)),
        }
    }
}

impl CastFrom<Int> for Complex {
    fn cast_from(i: Int) -> Self {
        i.into()
    }
}

impl From<UInt> for Complex {
    fn from(u: UInt) -> Self {
        match u {
            UInt::U64(u) => Self::C64(num::Complex::new(u as f64, 0.0f64)),
            UInt::U32(u) => Self::C32(num::Complex::new(u as f32, 0.0f32)),
            UInt::U16(u) => Self::C32(num::Complex::new(u as f32, 0.0f32)),
            UInt::U8(u) => Self::C32(num::Complex::new(u as f32, 0.0f32)),
        }
    }
}

impl CastFrom<UInt> for Complex {
    fn cast_from(u: UInt) -> Self {
        u.into()
    }
}

impl From<bool> for Complex {
    fn from(b: bool) -> Self {
        if b {
            Self::C32(num::Complex::new(1.0f32, 0.0f32))
        } else {
            Self::C64(num::Complex::new(1.0f64, 0.0f64))
        }
    }
}

impl CastFrom<bool> for Complex {
    fn cast_from(b: bool) -> Complex {
        b.into()
    }
}

impl From<num::Complex<f32>> for Complex {
    fn from(c: num::Complex<f32>) -> Complex {
        Complex::C32(c)
    }
}

impl From<num::Complex<f64>> for Complex {
    fn from(c: num::Complex<f64>) -> Complex {
        Complex::C64(c)
    }
}

impl TryFrom<Complex> for num::Complex<f32> {
    type Error = error::TCError;

    fn try_from(c: Complex) -> TCResult<num::Complex<f32>> {
        match c {
            Complex::C32(c) => Ok(c),
            other => Err(error::bad_request("Expected C32 but found", other)),
        }
    }
}

impl Serialize for Complex {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Complex::C32(c) => {
                let mut map = s.serialize_map(Some(1))?;
                map.serialize_entry("/sbin/value/number/complex/32", &[[c.re, c.im]])?;
                map.end()
            }
            Complex::C64(c) => {
                let mut map = s.serialize_map(Some(1))?;
                map.serialize_entry("/sbin/value/number/complex/64", &[[c.re, c.im]])?;
                map.end()
            }
        }
    }
}

impl fmt::Display for Complex {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Complex::C32(c) => write!(f, "C32({})", c),
            Complex::C64(c) => write!(f, "C64({})", c),
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum Float {
    F32(f32),
    F64(f64),
}

impl Impl for Float {
    type Class = FloatType;

    fn class(&self) -> FloatType {
        match self {
            Float::F32(_) => FloatType::F32,
            Float::F64(_) => FloatType::F64,
        }
    }
}

impl ValueImpl for Float {
    type Class = FloatType;
}

impl NumberImpl for Float {
    type Abs = Float;
    type Class = FloatType;

    fn abs(&self) -> Float {
        match self {
            Self::F32(f) => Self::F32(f.abs()),
            Self::F64(f) => Self::F64(f.abs()),
        }
    }
}

impl CastFrom<Complex> for Float {
    fn cast_from(c: Complex) -> Float {
        use Complex::*;
        match c {
            C32(c) => Self::F32(c.re),
            C64(c) => Self::F64(c.re),
        }
    }
}

impl CastFrom<Float> for bool {
    fn cast_from(f: Float) -> bool {
        use Float::*;
        match f {
            F32(f) if f == 0f32 => false,
            F64(f) if f == 0f64 => false,
            _ => true,
        }
    }
}

impl Eq for Float {}

impl Add for Float {
    type Output = Self;

    fn add(self, other: Float) -> Self {
        match (self, other) {
            (Self::F32(l), Self::F32(r)) => Self::F32(l + r),
            (Self::F64(l), Self::F64(r)) => Self::F64(l + r),
            (Self::F64(l), Self::F32(r)) => Self::F64(l + r as f64),
            (l, r) => (r + l),
        }
    }
}

impl Mul for Float {
    type Output = Self;

    fn mul(self, other: Float) -> Self {
        match (self, other) {
            (Self::F32(l), Self::F32(r)) => Self::F32(l * r),
            (Self::F64(l), Self::F64(r)) => Self::F64(l * r),
            (Self::F64(l), Self::F32(r)) => Self::F64(l * r as f64),
            (l, r) => (r * l),
        }
    }
}

impl PartialOrd for Float {
    fn partial_cmp(&self, other: &Float) -> Option<Ordering> {
        match (self, other) {
            (Float::F32(l), Float::F32(r)) => l.partial_cmp(r),
            (Float::F64(l), Float::F64(r)) => l.partial_cmp(r),
            _ => None,
        }
    }
}

impl From<bool> for Float {
    fn from(b: bool) -> Self {
        if b {
            Self::F32(1.0f32)
        } else {
            Self::F32(0.0f32)
        }
    }
}

impl CastFrom<bool> for Float {
    fn cast_from(b: bool) -> Self {
        b.into()
    }
}

impl From<f32> for Float {
    fn from(f: f32) -> Self {
        Self::F32(f)
    }
}

impl From<f64> for Float {
    fn from(f: f64) -> Self {
        Self::F64(f)
    }
}

impl From<Int> for Float {
    fn from(i: Int) -> Self {
        match i {
            Int::I64(i) => Self::F64(i as f64),
            Int::I32(i) => Self::F32(i as f32),
            Int::I16(i) => Self::F32(i as f32),
        }
    }
}

impl CastFrom<Int> for Float {
    fn cast_from(i: Int) -> Self {
        i.into()
    }
}

impl From<UInt> for Float {
    fn from(u: UInt) -> Self {
        match u {
            UInt::U64(u) => Self::F64(u as f64),
            UInt::U32(u) => Self::F32(u as f32),
            UInt::U16(u) => Self::F32(u as f32),
            UInt::U8(u) => Self::F32(u as f32),
        }
    }
}

impl CastFrom<UInt> for Float {
    fn cast_from(u: UInt) -> Self {
        u.into()
    }
}

impl TryFrom<Float> for f32 {
    type Error = error::TCError;

    fn try_from(f: Float) -> TCResult<f32> {
        match f {
            Float::F32(f) => Ok(f),
            other => Err(error::bad_request("Expected F32 but found", other)),
        }
    }
}

impl From<Float> for f64 {
    fn from(f: Float) -> f64 {
        match f {
            Float::F32(f) => f as f64,
            Float::F64(f) => f,
        }
    }
}

impl Serialize for Float {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Float::F32(f) => s.serialize_f32(*f),
            Float::F64(f) => s.serialize_f64(*f),
        }
    }
}

impl fmt::Display for Float {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Float::F32(i) => write!(f, "F32({})", i),
            Float::F64(i) => write!(f, "F64({})", i),
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum Int {
    I16(i16),
    I32(i32),
    I64(i64),
}

impl Impl for Int {
    type Class = IntType;

    fn class(&self) -> IntType {
        match self {
            Int::I16(_) => IntType::I16,
            Int::I32(_) => IntType::I32,
            Int::I64(_) => IntType::I64,
        }
    }
}

impl ValueImpl for Int {
    type Class = IntType;
}

impl NumberImpl for Int {
    type Abs = Self;
    type Class = IntType;

    fn abs(&self) -> Self {
        match self {
            Self::I16(i) => Int::I16(i.abs()),
            Self::I32(i) => Int::I32(i.abs()),
            Self::I64(i) => Int::I64(i.abs()),
        }
    }
}

impl CastFrom<Complex> for Int {
    fn cast_from(c: Complex) -> Int {
        use Complex::*;
        match c {
            C32(c) => Self::I32(c.re as i32),
            C64(c) => Self::I64(c.re as i64),
        }
    }
}

impl CastFrom<Float> for Int {
    fn cast_from(f: Float) -> Int {
        use Float::*;
        match f {
            F32(f) => Self::I32(f as i32),
            F64(f) => Self::I64(f as i64),
        }
    }
}

impl CastFrom<Int> for bool {
    fn cast_from(i: Int) -> bool {
        use Int::*;
        match i {
            I16(i) if i == 0i16 => false,
            I32(i) if i == 0i32 => false,
            I64(i) if i == 0i64 => false,
            _ => true,
        }
    }
}

impl Eq for Int {}

impl Add for Int {
    type Output = Self;

    fn add(self, other: Int) -> Self {
        match (self, other) {
            (Self::I64(l), Self::I64(r)) => Self::I64(l + r),
            (Self::I64(l), Self::I32(r)) => Self::I64(l + r as i64),
            (Self::I64(l), Self::I16(r)) => Self::I64(l + r as i64),
            (Self::I32(l), Self::I32(r)) => Self::I32(l + r),
            (Self::I32(l), Self::I16(r)) => Self::I32(l + r as i32),
            (Self::I16(l), Self::I16(r)) => Self::I16(l + r),
            (l, r) => r + l,
        }
    }
}

impl Mul for Int {
    type Output = Self;

    fn mul(self, other: Int) -> Self {
        match (self, other) {
            (Self::I64(l), Self::I64(r)) => Self::I64(l * r),
            (Self::I64(l), Self::I32(r)) => Self::I64(l * r as i64),
            (Self::I64(l), Self::I16(r)) => Self::I64(l * r as i64),
            (Self::I32(l), Self::I32(r)) => Self::I32(l * r),
            (Self::I32(l), Self::I16(r)) => Self::I32(l * r as i32),
            (Self::I16(l), Self::I16(r)) => Self::I16(l * r),
            (l, r) => r * l,
        }
    }
}

impl PartialOrd for Int {
    fn partial_cmp(&self, other: &Int) -> Option<Ordering> {
        match (self, other) {
            (Int::I16(l), Int::I16(r)) => l.partial_cmp(r),
            (Int::I32(l), Int::I32(r)) => l.partial_cmp(r),
            (Int::I64(l), Int::I64(r)) => l.partial_cmp(r),
            _ => None,
        }
    }
}

impl From<i16> for Int {
    fn from(i: i16) -> Int {
        Int::I16(i)
    }
}

impl From<i32> for Int {
    fn from(i: i32) -> Int {
        Int::I32(i)
    }
}

impl From<i64> for Int {
    fn from(i: i64) -> Int {
        Int::I64(i)
    }
}

impl From<UInt> for Int {
    fn from(u: UInt) -> Int {
        match u {
            UInt::U64(u) => Int::I64(u as i64),
            UInt::U32(u) => Int::I32(u as i32),
            UInt::U16(u) => Int::I16(u as i16),
            UInt::U8(u) => Int::I16(u as i16),
        }
    }
}

impl CastFrom<UInt> for Int {
    fn cast_from(u: UInt) -> Int {
        u.into()
    }
}

impl From<bool> for Int {
    fn from(b: bool) -> Int {
        if b {
            Int::I16(1)
        } else {
            Int::I16(0)
        }
    }
}

impl CastFrom<bool> for Int {
    fn cast_from(b: bool) -> Int {
        b.into()
    }
}

impl TryFrom<Int> for i16 {
    type Error = error::TCError;

    fn try_from(i: Int) -> TCResult<i16> {
        match i {
            Int::I16(i) => Ok(i),
            other => Err(error::bad_request("Expected I16 but found", other)),
        }
    }
}

impl TryFrom<Int> for i32 {
    type Error = error::TCError;

    fn try_from(i: Int) -> TCResult<i32> {
        match i {
            Int::I32(i) => Ok(i),
            other => Err(error::bad_request("Expected I32 but found", other)),
        }
    }
}

impl From<Int> for i64 {
    fn from(i: Int) -> i64 {
        match i {
            Int::I16(i) => i as i64,
            Int::I32(i) => i as i64,
            Int::I64(i) => i,
        }
    }
}

impl Serialize for Int {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Int::I16(i) => s.serialize_i16(*i),
            Int::I32(i) => s.serialize_i32(*i),
            Int::I64(i) => s.serialize_i64(*i),
        }
    }
}

impl fmt::Display for Int {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Int::I16(i) => write!(f, "I16: {}", i),
            Int::I32(i) => write!(f, "I32: {}", i),
            Int::I64(i) => write!(f, "I64: {}", i),
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum UInt {
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
}

impl Impl for UInt {
    type Class = UIntType;

    fn class(&self) -> UIntType {
        match self {
            UInt::U8(_) => UIntType::U8,
            UInt::U16(_) => UIntType::U16,
            UInt::U32(_) => UIntType::U32,
            UInt::U64(_) => UIntType::U64,
        }
    }
}

impl ValueImpl for UInt {
    type Class = UIntType;
}

impl NumberImpl for UInt {
    type Abs = Self;
    type Class = UIntType;

    fn abs(&self) -> UInt {
        self.clone()
    }
}

impl CastFrom<Complex> for UInt {
    fn cast_from(c: Complex) -> UInt {
        use Complex::*;
        match c {
            C32(c) => Self::U32(c.re as u32),
            C64(c) => Self::U64(c.re as u64),
        }
    }
}

impl CastFrom<Float> for UInt {
    fn cast_from(f: Float) -> UInt {
        use Float::*;
        match f {
            F32(f) => Self::U32(f as u32),
            F64(f) => Self::U64(f as u64),
        }
    }
}

impl CastFrom<Int> for UInt {
    fn cast_from(i: Int) -> UInt {
        use Int::*;
        match i {
            I16(i) => Self::U16(i as u16),
            I32(i) => Self::U32(i as u32),
            I64(i) => Self::U64(i as u64),
        }
    }
}

impl CastFrom<UInt> for bool {
    fn cast_from(u: UInt) -> bool {
        use UInt::*;
        match u {
            U8(u) if u == 0u8 => false,
            U16(u) if u == 0u16 => false,
            U32(u) if u == 0u32 => false,
            U64(u) if u == 0u64 => false,
            _ => true,
        }
    }
}

impl Add for UInt {
    type Output = Self;

    fn add(self, other: UInt) -> Self {
        match (self, other) {
            (UInt::U64(l), UInt::U64(r)) => UInt::U64(l + r),
            (UInt::U64(l), UInt::U32(r)) => UInt::U64(l + r as u64),
            (UInt::U64(l), UInt::U16(r)) => UInt::U64(l + r as u64),
            (UInt::U64(l), UInt::U8(r)) => UInt::U64(l + r as u64),
            (UInt::U32(l), UInt::U32(r)) => UInt::U32(l + r),
            (UInt::U32(l), UInt::U16(r)) => UInt::U32(l + r as u32),
            (UInt::U32(l), UInt::U8(r)) => UInt::U32(l + r as u32),
            (UInt::U16(l), UInt::U16(r)) => UInt::U16(l + r),
            (UInt::U16(l), UInt::U8(r)) => UInt::U16(l + r as u16),
            (UInt::U8(l), UInt::U8(r)) => UInt::U8(l + r),
            (l, r) => r + l,
        }
    }
}

impl Mul for UInt {
    type Output = Self;

    fn mul(self, other: UInt) -> Self {
        match (self, other) {
            (UInt::U64(l), UInt::U64(r)) => UInt::U64(l * r),
            (UInt::U64(l), UInt::U32(r)) => UInt::U64(l * r as u64),
            (UInt::U64(l), UInt::U16(r)) => UInt::U64(l * r as u64),
            (UInt::U64(l), UInt::U8(r)) => UInt::U64(l * r as u64),
            (UInt::U32(l), UInt::U32(r)) => UInt::U32(l * r),
            (UInt::U32(l), UInt::U16(r)) => UInt::U32(l * r as u32),
            (UInt::U32(l), UInt::U8(r)) => UInt::U32(l * r as u32),
            (UInt::U16(l), UInt::U16(r)) => UInt::U16(l * r),
            (UInt::U16(l), UInt::U8(r)) => UInt::U16(l * r as u16),
            (UInt::U8(l), UInt::U8(r)) => UInt::U8(l * r),
            (l, r) => r * l,
        }
    }
}

impl Eq for UInt {}

impl Ord for UInt {
    fn cmp(&self, other: &UInt) -> Ordering {
        match (self, other) {
            (UInt::U64(l), UInt::U64(r)) => l.cmp(r),
            (UInt::U64(l), UInt::U32(r)) => l.cmp(&r.clone().into()),
            (UInt::U64(l), UInt::U16(r)) => l.cmp(&r.clone().into()),
            (UInt::U64(l), UInt::U8(r)) => l.cmp(&r.clone().into()),
            (UInt::U32(l), UInt::U32(r)) => l.cmp(r),
            (UInt::U32(l), UInt::U16(r)) => l.cmp(&r.clone().into()),
            (UInt::U32(l), UInt::U8(r)) => l.cmp(&r.clone().into()),
            (UInt::U16(l), UInt::U16(r)) => l.cmp(r),
            (UInt::U16(l), UInt::U8(r)) => l.cmp(&r.clone().into()),
            (UInt::U8(l), UInt::U8(r)) => l.cmp(r),
            (l, r) => match r.cmp(l) {
                Ordering::Greater => Ordering::Less,
                Ordering::Less => Ordering::Greater,
                Ordering::Equal => Ordering::Equal,
            },
        }
    }
}

impl PartialOrd for UInt {
    fn partial_cmp(&self, other: &UInt) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl From<bool> for UInt {
    fn from(b: bool) -> UInt {
        if b {
            UInt::U8(1)
        } else {
            UInt::U8(0)
        }
    }
}

impl CastFrom<bool> for UInt {
    fn cast_from(b: bool) -> UInt {
        b.into()
    }
}

impl From<u8> for UInt {
    fn from(u: u8) -> UInt {
        UInt::U8(u)
    }
}

impl From<u16> for UInt {
    fn from(u: u16) -> UInt {
        UInt::U16(u)
    }
}

impl From<u32> for UInt {
    fn from(u: u32) -> UInt {
        UInt::U32(u)
    }
}

impl From<u64> for UInt {
    fn from(u: u64) -> UInt {
        UInt::U64(u)
    }
}

impl TryFrom<UInt> for u8 {
    type Error = error::TCError;

    fn try_from(u: UInt) -> TCResult<u8> {
        match u {
            UInt::U8(u) => Ok(u),
            other => Err(error::bad_request("Expected a UInt8 but found", other)),
        }
    }
}

impl TryFrom<UInt> for u16 {
    type Error = error::TCError;

    fn try_from(u: UInt) -> TCResult<u16> {
        match u {
            UInt::U16(u) => Ok(u),
            other => Err(error::bad_request("Expected a UInt16 but found", other)),
        }
    }
}

impl TryFrom<UInt> for u32 {
    type Error = error::TCError;

    fn try_from(u: UInt) -> TCResult<u32> {
        match u {
            UInt::U32(u) => Ok(u),
            other => Err(error::bad_request("Expected a UInt32 but found", other)),
        }
    }
}

impl TryFrom<UInt> for u64 {
    type Error = error::TCError;

    fn try_from(u: UInt) -> TCResult<u64> {
        match u {
            UInt::U64(u) => Ok(u),
            other => Err(error::bad_request("Expected a UInt64 but found", other)),
        }
    }
}

impl Serialize for UInt {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            UInt::U8(u) => s.serialize_u8(*u),
            UInt::U16(u) => s.serialize_u16(*u),
            UInt::U32(u) => s.serialize_u32(*u),
            UInt::U64(u) => s.serialize_u64(*u),
        }
    }
}

impl fmt::Display for UInt {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UInt::U8(u) => write!(f, "U8: {}", u),
            UInt::U16(u) => write!(f, "UInt16: {}", u),
            UInt::U32(u) => write!(f, "UInt32: {}", u),
            UInt::U64(u) => write!(f, "UInt64: {}", u),
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum Number {
    Bool(bool),
    Complex(Complex),
    Float(Float),
    Int(Int),
    UInt(UInt),
}

impl PartialOrd for Number {
    fn partial_cmp(&self, other: &Number) -> Option<Ordering> {
        match (self, other) {
            (Self::Complex(l), Self::Complex(r)) => l.partial_cmp(r),
            (Self::Complex(l), Self::Float(r)) => l.partial_cmp(&r.clone().into()),
            (Self::Complex(l), Self::Int(r)) => l.partial_cmp(&r.clone().into()),
            (Self::Complex(l), Self::UInt(r)) => l.partial_cmp(&r.clone().into()),
            (Self::Complex(l), Self::Bool(r)) => l.partial_cmp(&r.clone().into()),
            (Self::Float(l), Self::Float(r)) => l.partial_cmp(r),
            (Self::Float(l), Self::Int(r)) => l.partial_cmp(&r.clone().into()),
            (Self::Float(l), Self::UInt(r)) => l.partial_cmp(&r.clone().into()),
            (Self::Float(l), Self::Bool(r)) => l.partial_cmp(&r.clone().into()),
            (Self::Int(l), Self::Int(r)) => l.partial_cmp(r),
            (Self::Int(l), Self::UInt(r)) => l.partial_cmp(&r.clone().into()),
            (Self::UInt(l), Self::UInt(r)) => l.partial_cmp(r),
            (Self::Bool(l), Self::Bool(r)) => l.partial_cmp(r),
            (l, r) => match r.partial_cmp(l) {
                Some(Ordering::Greater) => Some(Ordering::Less),
                Some(Ordering::Less) => Some(Ordering::Greater),
                Some(Ordering::Equal) => Some(Ordering::Equal),
                None => None,
            },
        }
    }
}

impl Impl for Number {
    type Class = NumberType;

    fn class(&self) -> NumberType {
        use NumberType::*;
        match self {
            Self::Bool(_) => Bool,
            Self::Complex(c) => Complex(c.class()),
            Self::Float(f) => Float(f.class()),
            Self::Int(i) => Int(i.class()),
            Self::UInt(u) => UInt(u.class()),
        }
    }
}

impl ValueImpl for Number {
    type Class = NumberType;
}

impl NumberImpl for Number {
    type Abs = Number;
    type Class = NumberType;

    fn abs(&self) -> Number {
        use Number::*;
        match self {
            Complex(c) => Float(c.abs()),
            Float(f) => Float(f.abs()),
            Int(i) => Int(i.abs()),
            other => other.clone(),
        }
    }
}

impl CastFrom<Number> for bool {
    fn cast_from(number: Number) -> bool {
        use Number::*;
        match number {
            Bool(b) => b,
            Complex(c) => bool::cast_from(c),
            Float(f) => bool::cast_from(f),
            Int(i) => bool::cast_from(i),
            UInt(u) => bool::cast_from(u),
        }
    }
}

impl CastFrom<Number> for Float {
    fn cast_from(number: Number) -> Float {
        use Number::*;
        match number {
            Bool(b) => Self::cast_from(b),
            Complex(c) => Self::cast_from(c),
            Float(f) => f,
            Int(i) => Self::cast_from(i),
            UInt(u) => Self::cast_from(u),
        }
    }
}

impl CastFrom<Number> for Int {
    fn cast_from(number: Number) -> Int {
        use Number::*;
        match number {
            Bool(b) => Self::cast_from(b),
            Complex(c) => Self::cast_from(c),
            Float(f) => Self::cast_from(f),
            Int(i) => i,
            UInt(u) => Self::cast_from(u),
        }
    }
}

impl CastFrom<Number> for UInt {
    fn cast_from(number: Number) -> UInt {
        use Number::*;
        match number {
            Bool(b) => Self::cast_from(b),
            Complex(c) => Self::cast_from(c),
            Float(f) => Self::cast_from(f),
            Int(i) => Self::cast_from(i),
            UInt(u) => u,
        }
    }
}

impl Add for Number {
    type Output = Self;

    fn add(self, other: Number) -> Self {
        match (self, other) {
            (Self::Bool(l), Self::Bool(r)) => match (l, r) {
                (true, true) => Self::UInt(UInt::U8(2)),
                (true, false) => Self::UInt(UInt::U8(1)),
                (false, true) => Self::UInt(UInt::U8(1)),
                (false, false) => Self::UInt(UInt::U8(0)),
            },

            (Self::Complex(l), Self::Complex(r)) => Self::Complex(l + r),
            (Self::Complex(l), Self::Float(r)) => {
                let r: Complex = r.into();
                Self::Complex(l + r)
            }
            (Self::Complex(l), Self::Int(r)) => {
                let r: Complex = r.into();
                Self::Complex(l + r)
            }
            (Self::Complex(l), Self::UInt(r)) => {
                let r: Complex = r.into();
                Self::Complex(l + r)
            }
            (Self::Complex(l), Self::Bool(r)) => {
                let r: Complex = r.into();
                Self::Complex(l + r)
            }
            (Self::Float(l), Self::Float(r)) => Self::Float(l + r),
            (Self::Float(l), Self::Int(r)) => {
                let r: Float = r.into();
                Self::Float(l + r)
            }
            (Self::Float(l), Self::UInt(r)) => {
                let r: Float = r.into();
                Self::Float(l + r)
            }
            (Self::Float(l), Self::Bool(r)) => {
                let r: Float = r.into();
                Self::Float(l + r)
            }
            (Self::Int(l), Self::Int(r)) => Self::Int(l + r),
            (Self::Int(l), Self::UInt(r)) => {
                let r: Int = r.into();
                Self::Int(l + r)
            }
            (Self::Int(l), Self::Bool(r)) => {
                let r: Int = r.into();
                Self::Int(l + r)
            }
            (Self::UInt(l), Self::UInt(r)) => Self::UInt(l + r),
            (Self::UInt(l), Self::Bool(r)) => {
                let r: UInt = r.into();
                Self::UInt(l + r)
            }
            (l, r) => r + l,
        }
    }
}

impl Mul for Number {
    type Output = Self;

    fn mul(self, other: Number) -> Self {
        match (self, other) {
            (Self::Bool(false), r) => r.class().zero(),
            (Self::Bool(true), r) => r,

            (Self::Complex(l), Self::Complex(r)) => Self::Complex(l * r),
            (Self::Complex(l), Self::Float(r)) => {
                let r: Complex = r.into();
                Self::Complex(l * r)
            }
            (Self::Complex(l), Self::Int(r)) => {
                let r: Complex = r.into();
                Self::Complex(l * r)
            }
            (Self::Complex(l), Self::UInt(r)) => {
                let r: Complex = r.into();
                Self::Complex(l * r)
            }
            (Self::Float(l), Self::Float(r)) => Self::Float(l * r),
            (Self::Float(l), Self::Int(r)) => {
                let r: Float = r.into();
                Self::Float(l * r)
            }
            (Self::Float(l), Self::UInt(r)) => {
                let r: Float = r.into();
                Self::Float(l * r)
            }
            (Self::Int(l), Self::Int(r)) => Self::Int(l * r),
            (Self::Int(l), Self::UInt(r)) => {
                let r: Int = r.into();
                Self::Int(l * r)
            }
            (Self::UInt(l), Self::UInt(r)) => Self::UInt(l * r),
            (l, r) => r * l,
        }
    }
}

impl From<bool> for Number {
    fn from(b: bool) -> Number {
        Number::Bool(b)
    }
}

pub trait Numeric {}

impl From<Complex> for Number {
    fn from(c: Complex) -> Number {
        Number::Complex(c)
    }
}

impl From<Float> for Number {
    fn from(f: Float) -> Number {
        Number::Float(f)
    }
}

impl From<Int> for Number {
    fn from(i: Int) -> Number {
        Number::Int(i)
    }
}

impl From<UInt> for Number {
    fn from(u: UInt) -> Number {
        Number::UInt(u)
    }
}

impl TryFrom<Number> for bool {
    type Error = error::TCError;

    fn try_from(n: Number) -> TCResult<bool> {
        match n {
            Number::Bool(b) => Ok(b),
            other => Err(error::bad_request("Expected Bool but found", other)),
        }
    }
}

impl TryFrom<Number> for Complex {
    type Error = error::TCError;

    fn try_from(n: Number) -> TCResult<Complex> {
        match n {
            Number::Complex(c) => Ok(c),
            other => Err(error::bad_request("Expected Complex but found", other)),
        }
    }
}

impl TryFrom<Number> for Float {
    type Error = error::TCError;

    fn try_from(n: Number) -> TCResult<Float> {
        match n {
            Number::Float(f) => Ok(f),
            other => Err(error::bad_request("Expected Float but found", other)),
        }
    }
}

impl TryFrom<Number> for Int {
    type Error = error::TCError;

    fn try_from(n: Number) -> TCResult<Int> {
        match n {
            Number::Int(i) => Ok(i),
            other => Err(error::bad_request("Expected Int but found", other)),
        }
    }
}

impl TryFrom<Number> for UInt {
    type Error = error::TCError;

    fn try_from(n: Number) -> TCResult<UInt> {
        match n {
            Number::UInt(u) => Ok(u),
            other => Err(error::bad_request("Expected UInt but found", other)),
        }
    }
}

impl Serialize for Number {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Number::Bool(b) => s.serialize_bool(*b),
            Number::Complex(c) => c.serialize(s),
            Number::Float(f) => f.serialize(s),
            Number::Int(i) => i.serialize(s),
            Number::UInt(u) => u.serialize(s),
        }
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Number::Bool(b) => write!(f, "Bool({})", b),
            Number::Complex(c) => write!(f, "Complex({})", c),
            Number::Float(n) => write!(f, "Float({})", n),
            Number::Int(i) => write!(f, "Int({})", i),
            Number::UInt(u) => write!(f, "UInt({})", u),
        }
    }
}

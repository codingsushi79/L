//! The L type representation (SPEC §9–§15, §29, §30).

use std::fmt;

/// A definition identifier: a struct, enum, interface, function or constant,
/// unique across the whole compilation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct DefId(pub u32);

/// A local binding inside one function body.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct LocalId(pub u32);

/// A type variable, used while inferring (SPEC §7 type inference).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct TyVar(pub u32);

/// The primitive types of SPEC §9.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Prim {
    Bool,
    Char,
    Str,
    Byte,
    Int,
    Int8,
    Int16,
    Int32,
    Int64,
    Int128,
    Uint,
    Uint8,
    Uint16,
    Uint32,
    Uint64,
    Uint128,
    Float,
    Float32,
    Float64,
}

impl Prim {
    /// Look up a primitive by the name written in source.
    pub fn from_name(name: &str) -> Option<Prim> {
        use Prim::*;
        Some(match name {
            "bool" => Bool,
            "char" => Char,
            "str" => Str,
            "byte" => Byte,
            "int" => Int,
            "int8" => Int8,
            "int16" => Int16,
            "int32" => Int32,
            "int64" => Int64,
            "int128" => Int128,
            "uint" => Uint,
            "uint8" => Uint8,
            "uint16" => Uint16,
            "uint32" => Uint32,
            "uint64" => Uint64,
            "uint128" => Uint128,
            "float" => Float,
            "float32" => Float32,
            "float64" => Float64,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        use Prim::*;
        match self {
            Bool => "bool",
            Char => "char",
            Str => "str",
            Byte => "byte",
            Int => "int",
            Int8 => "int8",
            Int16 => "int16",
            Int32 => "int32",
            Int64 => "int64",
            Int128 => "int128",
            Uint => "uint",
            Uint8 => "uint8",
            Uint16 => "uint16",
            Uint32 => "uint32",
            Uint64 => "uint64",
            Uint128 => "uint128",
            Float => "float",
            Float32 => "float32",
            Float64 => "float64",
        }
    }

    /// Also accepts the short suffix spellings, e.g. `i64` for `int64`.
    pub fn from_suffix(suffix: &str) -> Option<Prim> {
        use Prim::*;
        Some(match suffix {
            "i8" => Int8,
            "i16" => Int16,
            "i32" => Int32,
            "i64" => Int64,
            "i128" => Int128,
            "u8" => Uint8,
            "u16" => Uint16,
            "u32" => Uint32,
            "u64" => Uint64,
            "u128" => Uint128,
            "f32" => Float32,
            "f64" => Float64,
            other => return Prim::from_name(other),
        })
    }

    pub fn is_signed_int(self) -> bool {
        use Prim::*;
        matches!(self, Int | Int8 | Int16 | Int32 | Int64 | Int128)
    }

    pub fn is_unsigned_int(self) -> bool {
        use Prim::*;
        matches!(self, Byte | Uint | Uint8 | Uint16 | Uint32 | Uint64 | Uint128)
    }

    pub fn is_integer(self) -> bool {
        self.is_signed_int() || self.is_unsigned_int()
    }

    pub fn is_float(self) -> bool {
        use Prim::*;
        matches!(self, Float | Float32 | Float64)
    }

    pub fn is_numeric(self) -> bool {
        self.is_integer() || self.is_float()
    }

    /// Width in bits. `int` and `uint` are platform-native (SPEC §9); the
    /// reference compiler targets 64-bit platforms.
    pub fn bit_width(self) -> Option<u32> {
        use Prim::*;
        Some(match self {
            Bool => 8,
            Byte | Int8 | Uint8 => 8,
            Char => 32,
            Int16 | Uint16 => 16,
            Int32 | Uint32 | Float32 => 32,
            Int | Uint | Int64 | Uint64 | Float | Float64 => 64,
            Int128 | Uint128 => 128,
            Str => return None,
        })
    }

    /// The inclusive range of values a signed integer type can hold.
    pub fn signed_range(self) -> Option<(i128, i128)> {
        if !self.is_signed_int() {
            return None;
        }
        let bits = self.bit_width()?;
        if bits >= 128 {
            return Some((i128::MIN, i128::MAX));
        }
        let max = (1i128 << (bits - 1)) - 1;
        Some((-max - 1, max))
    }

    /// The inclusive maximum an unsigned integer type can hold.
    pub fn unsigned_max(self) -> Option<u128> {
        if !self.is_unsigned_int() {
            return None;
        }
        let bits = self.bit_width()?;
        if bits >= 128 {
            return Some(u128::MAX);
        }
        Some((1u128 << bits) - 1)
    }
}

impl fmt::Display for Prim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A resolved type.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Ty {
    Prim(Prim),
    /// The type of an expression producing no value (SPEC §16).
    Void,
    /// `T[]` (SPEC §12).
    Array(Box<Ty>),
    /// `map<K, V>` (SPEC §13).
    Map(Box<Ty>, Box<Ty>),
    /// `set<T>` (SPEC §14).
    Set(Box<Ty>),
    /// `(A, B)` (SPEC §15).
    Tuple(Vec<Ty>),
    /// `T?` (SPEC §30).
    Optional(Box<Ty>),
    /// A struct or enum, with its generic arguments applied.
    Adt { def: DefId, args: Vec<Ty> },
    /// An interface used as a type (SPEC §28).
    Interface { def: DefId, args: Vec<Ty> },
    /// A generic parameter still in scope, e.g. `T` inside `fn first<T>`.
    Param { def: DefId, index: u32, name: String },
    /// A function value.
    Fn { params: Vec<Ty>, ret: Box<Ty> },
    /// A range, produced by `a..b` (SPEC §19).
    Range(Box<Ty>),
    /// The result of `spawn` (SPEC §68).
    Task(Box<Ty>),
    /// A channel (SPEC §69).
    Channel(Box<Ty>),
    /// An unresolved inference variable.
    Infer(TyVar),
    /// A type that could not be determined; suppresses further errors.
    Err,
}

impl Ty {
    pub const BOOL: Ty = Ty::Prim(Prim::Bool);
    pub const INT: Ty = Ty::Prim(Prim::Int);
    pub const STR: Ty = Ty::Prim(Prim::Str);
    pub const FLOAT: Ty = Ty::Prim(Prim::Float);
    pub const CHAR: Ty = Ty::Prim(Prim::Char);

    pub fn is_err(&self) -> bool {
        matches!(self, Ty::Err)
    }

    pub fn is_void(&self) -> bool {
        matches!(self, Ty::Void)
    }

    pub fn is_optional(&self) -> bool {
        matches!(self, Ty::Optional(_))
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Ty::Prim(p) if p.is_numeric())
    }

    pub fn is_integer(&self) -> bool {
        matches!(self, Ty::Prim(p) if p.is_integer())
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Ty::Prim(p) if p.is_float())
    }

    pub fn is_bool(&self) -> bool {
        matches!(self, Ty::Prim(Prim::Bool))
    }

    pub fn is_str(&self) -> bool {
        matches!(self, Ty::Prim(Prim::Str))
    }

    /// The type inside a `T?`, or `self` if it is not optional.
    pub fn unwrap_optional(&self) -> &Ty {
        match self {
            Ty::Optional(inner) => inner,
            other => other,
        }
    }

    /// Wrap in `Optional` unless already optional; `T??` is not a type.
    pub fn into_optional(self) -> Ty {
        match self {
            Ty::Optional(_) => self,
            other => Ty::Optional(Box::new(other)),
        }
    }

    /// The element type produced by iterating this type (SPEC §19).
    pub fn iter_element(&self) -> Option<Ty> {
        match self {
            Ty::Array(inner) | Ty::Set(inner) | Ty::Range(inner) => Some((**inner).clone()),
            // `for k in map` iterates keys.
            Ty::Map(k, _) => Some((**k).clone()),
            Ty::Prim(Prim::Str) => Some(Ty::Prim(Prim::Char)),
            // `for i in 10` counts (SPEC §19).
            Ty::Prim(p) if p.is_integer() => Some(Ty::Prim(*p)),
            _ => None,
        }
    }

    /// Whether the type mentions an inference variable.
    pub fn has_infer(&self) -> bool {
        match self {
            Ty::Infer(_) => true,
            Ty::Array(t) | Ty::Set(t) | Ty::Optional(t) | Ty::Range(t) | Ty::Task(t)
            | Ty::Channel(t) => t.has_infer(),
            Ty::Map(k, v) => k.has_infer() || v.has_infer(),
            Ty::Tuple(items) => items.iter().any(Ty::has_infer),
            Ty::Adt { args, .. } | Ty::Interface { args, .. } => args.iter().any(Ty::has_infer),
            Ty::Fn { params, ret } => params.iter().any(Ty::has_infer) || ret.has_infer(),
            _ => false,
        }
    }

    /// Render for diagnostics. `names` resolves definition identifiers.
    pub fn render(&self, names: &dyn Fn(DefId) -> String) -> String {
        match self {
            Ty::Prim(p) => p.name().to_string(),
            Ty::Void => "void".into(),
            Ty::Array(t) => format!("{}[]", t.render(names)),
            Ty::Map(k, v) => format!("map<{}, {}>", k.render(names), v.render(names)),
            Ty::Set(t) => format!("set<{}>", t.render(names)),
            Ty::Tuple(items) => {
                let parts: Vec<_> = items.iter().map(|t| t.render(names)).collect();
                format!("({})", parts.join(", "))
            }
            Ty::Optional(t) => format!("{}?", t.render(names)),
            Ty::Adt { def, args } | Ty::Interface { def, args } => {
                let base = names(*def);
                if args.is_empty() {
                    base
                } else {
                    let parts: Vec<_> = args.iter().map(|t| t.render(names)).collect();
                    format!("{base}<{}>", parts.join(", "))
                }
            }
            Ty::Param { name, .. } => name.clone(),
            Ty::Fn { params, ret } => {
                let parts: Vec<_> = params.iter().map(|t| t.render(names)).collect();
                if ret.is_void() {
                    format!("fn({})", parts.join(", "))
                } else {
                    format!("fn({}) -> {}", parts.join(", "), ret.render(names))
                }
            }
            Ty::Range(t) => format!("range<{}>", t.render(names)),
            Ty::Task(t) => format!("task<{}>", t.render(names)),
            Ty::Channel(t) => format!("channel<{}>", t.render(names)),
            Ty::Infer(_) => "_".into(),
            Ty::Err => "<error>".into(),
        }
    }
}

impl fmt::Display for Ty {
    /// Renders without a definition table, so ADTs show as `#id`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render(&|d: DefId| format!("#{}", d.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_primitive_names() {
        assert_eq!(Prim::from_name("int"), Some(Prim::Int));
        assert_eq!(Prim::from_name("float64"), Some(Prim::Float64));
        assert_eq!(Prim::from_name("nope"), None);
        assert_eq!(Prim::Int.name(), "int");
    }

    #[test]
    fn maps_literal_suffixes() {
        assert_eq!(Prim::from_suffix("i64"), Some(Prim::Int64));
        assert_eq!(Prim::from_suffix("u8"), Some(Prim::Uint8));
        assert_eq!(Prim::from_suffix("f32"), Some(Prim::Float32));
        assert_eq!(Prim::from_suffix("int"), Some(Prim::Int));
    }

    #[test]
    fn classifies_primitives() {
        assert!(Prim::Int.is_signed_int());
        assert!(Prim::Uint8.is_unsigned_int());
        assert!(Prim::Byte.is_unsigned_int());
        assert!(Prim::Float32.is_float());
        assert!(Prim::Int.is_numeric());
        assert!(!Prim::Str.is_numeric());
        assert!(!Prim::Bool.is_numeric());
    }

    #[test]
    fn reports_integer_ranges() {
        assert_eq!(Prim::Int8.signed_range(), Some((-128, 127)));
        assert_eq!(Prim::Int32.signed_range(), Some((-2147483648, 2147483647)));
        assert_eq!(Prim::Uint8.unsigned_max(), Some(255));
        assert_eq!(Prim::Uint128.unsigned_max(), Some(u128::MAX));
        assert_eq!(Prim::Int.unsigned_max(), None);
    }

    #[test]
    fn optionals_do_not_nest() {
        let t = Ty::INT.into_optional();
        assert_eq!(t.clone().into_optional(), t);
        assert_eq!(t.unwrap_optional(), &Ty::INT);
    }

    #[test]
    fn iteration_element_types() {
        // SPEC §19
        assert_eq!(Ty::Array(Box::new(Ty::INT)).iter_element(), Some(Ty::INT));
        assert_eq!(Ty::INT.iter_element(), Some(Ty::INT));
        assert_eq!(Ty::STR.iter_element(), Some(Ty::CHAR));
        assert_eq!(
            Ty::Map(Box::new(Ty::STR), Box::new(Ty::INT)).iter_element(),
            Some(Ty::STR)
        );
        assert_eq!(Ty::BOOL.iter_element(), None);
    }

    #[test]
    fn renders_types() {
        let names = |d: DefId| format!("T{}", d.0);
        assert_eq!(Ty::INT.render(&names), "int");
        assert_eq!(Ty::Array(Box::new(Ty::STR)).render(&names), "str[]");
        assert_eq!(Ty::INT.into_optional().render(&names), "int?");
        assert_eq!(
            Ty::Adt { def: DefId(3), args: vec![Ty::INT] }.render(&names),
            "T3<int>"
        );
        assert_eq!(
            Ty::Fn { params: vec![Ty::INT], ret: Box::new(Ty::Void) }.render(&names),
            "fn(int)"
        );
    }

    #[test]
    fn detects_inference_variables() {
        assert!(Ty::Infer(TyVar(0)).has_infer());
        assert!(Ty::Array(Box::new(Ty::Infer(TyVar(1)))).has_infer());
        assert!(!Ty::Array(Box::new(Ty::INT)).has_infer());
    }
}

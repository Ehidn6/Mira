//! Bounded portable scalar Mira evaluator. Contextual calls go through the graph
//! activation which owns this behavior; device and host OS effects stay unavailable.
use crate::{
    code::*,
    engine::{BehaviorHost, Calls, Detached},
    *,
};
#[derive(Clone, Debug)]
pub struct Vm {
    pub fuel: usize,
    depth: usize,
}
impl Default for Vm {
    fn default() -> Self {
        Self {
            fuel: 1_000_000,
            depth: 0,
        }
    }
}
impl Vm {
    fn step(&mut self) -> Result<()> {
        self.fuel = self
            .fuel
            .checked_sub(1)
            .ok_or("Mira behavior instruction budget")?;
        Ok(())
    }
    fn call_module(
        &mut self,
        modules: &[CodeModule],
        module: &CodeModule,
        args: &[i64],
        context: &mut dyn Calls,
    ) -> Result<i64> {
        self.step()?;
        if module.compile_time {
            return Err("compile-time macro cannot execute in the runtime VM".into());
        }
        if self.depth >= MAX_DEPTH || module.parameters.len() != args.len() {
            return Err("Mira behavior depth or arity error".into());
        }
        let mut env = BTreeMap::new();
        for (p, v) in module.parameters.iter().zip(args) {
            scalar(p.ty, *v)?;
            env.insert(p.name.clone(), *v);
        }
        self.depth += 1;
        let result = self.body(modules, module, &module.body, &mut env, context);
        self.depth -= 1;
        let value = result?;
        if module.output != Type::Unit {
            scalar(module.output, value)?;
        }
        Ok(value)
    }
    fn expr(
        &mut self,
        modules: &[CodeModule],
        owner: &CodeModule,
        e: &Expr,
        env: &mut BTreeMap<String, i64>,
        context: &mut dyn Calls,
    ) -> Result<i64> {
        self.step()?;
        match e {
            Expr::Hole(name)=>Err(format!("cannot execute code hole ?{name}")),
            Expr::Int(v)=>Ok(*v),Expr::Bool(v)=>Ok(i64::from(*v)),Expr::Unit=>Ok(0),
            Expr::Name(name)=>env.get(name).copied().ok_or_else(||format!("{name}: missing binding")),
            Expr::Unary(op,e)=>{
                let v=self.expr(modules,owner,e,env,context)?;
                Ok(match op{Unary::FNeg=>f32_word(-word_f32(v)?)?,Unary::Plus=>v,Unary::Neg=>v.checked_neg().ok_or("integer overflow")?,Unary::Not=>i64::from(v==0),Unary::BitNot=>!v})
            },
            Expr::Binary(op,a,b)=>{
                let a=self.expr(modules,owner,a,env,context)?;
                if *op==Binary::And && a==0{return Ok(0);}if *op==Binary::Or && a!=0{return Ok(1);}
                let b=self.expr(modules,owner,b,env,context)?;arithmetic(*op,a,b)
            },
            Expr::Choose(condition,yes,no)=>{
                let c=self.expr(modules,owner,condition,env,context)?;
                self.expr(modules,owner,if c!=0{yes}else{no},env,context)
            },
            Expr::Call(name,args)=>{
                if name.starts_with("sys.") { return Err(format!("{name}: system operation requires native kernel execution")); }
                let values:Vec<_>=args.iter().map(|e|self.expr(modules,owner,e,env,context)).collect::<Result<_>>()?;
                if name.starts_with("cells.") || name.starts_with("graph."){return context.invoke(self,name,&values);}
                let module=crate::code::resolve(modules,owner.id,name).ok_or_else(||format!("{name}: unavailable pure behavior"))?;
                self.call_module(modules,module,&values,context)
            },
            _=>Err("aggregate/address operations require the existing activation runtime; portable policies currently use scalars".into()),
        }
    }
    fn scope(
        &mut self,
        modules: &[CodeModule],
        owner: &CodeModule,
        body: &[Statement],
        env: &mut BTreeMap<String, i64>,
        context: &mut dyn Calls,
    ) -> Result<i64> {
        let mut inner = env.clone();
        let value = self.body(modules, owner, body, &mut inner, context)?;
        for (name, v) in env.iter_mut() {
            *v = inner[name];
        }
        Ok(value)
    }
    fn body(
        &mut self,
        modules: &[CodeModule],
        owner: &CodeModule,
        body: &[Statement],
        env: &mut BTreeMap<String, i64>,
        context: &mut dyn Calls,
    ) -> Result<i64> {
        let mut result = 0;
        for stmt in body {
            self.step()?;
            result = match stmt {
                Statement::Expr(e) => self.expr(modules, owner, e, env, context)?,
                Statement::Bind {
                    name, value, op, ..
                } => {
                    let rhs = self.expr(modules, owner, value, env, context)?;
                    let value = assignment(*op, env.get(name).copied(), rhs)?;
                    env.insert(name.clone(), value);
                    0
                }
                Statement::If { condition, yes, no } => {
                    let c = self.expr(modules, owner, condition, env, context)?;
                    self.scope(modules, owner, if c != 0 { yes } else { no }, env, context)?
                }
                Statement::While { condition, body } => {
                    while self.expr(modules, owner, condition, env, context)? != 0 {
                        self.step()?;
                        self.scope(modules, owner, body, env, context)?;
                    }
                    0
                }
                Statement::For {
                    name,
                    start,
                    end,
                    body,
                } => {
                    let start = self.expr(modules, owner, start, env, context)?;
                    let end = self.expr(modules, owner, end, env, context)?;
                    for i in start..end {
                        self.step()?;
                        let mut inner = env.clone();
                        inner.insert(name.clone(), i);
                        self.body(modules, owner, body, &mut inner, context)?;
                        for (n, v) in env.iter_mut() {
                            if n != name {
                                *v = inner[n];
                            }
                        }
                    }
                    0
                }
                Statement::Set { .. } => {
                    return Err(
                        "aggregate mutation is not available in scalar policy execution".into(),
                    );
                }
            };
        }
        Ok(result)
    }
}
impl BehaviorHost for Vm {
    fn call(&mut self, p: &Program, id: Id, cell: Option<Id>, args: &[i64]) -> Result<i64> {
        self.call_bound(p, id, cell, args, &mut Detached)
    }
    fn call_bound(
        &mut self,
        p: &Program,
        id: Id,
        _cell: Option<Id>,
        args: &[i64],
        context: &mut dyn Calls,
    ) -> Result<i64> {
        let module = crate::code::find(&p.code_modules, id)
            .ok_or("Mira behavior missing")?;
        self.call_module(&p.code_modules, module, args, context)
    }
}
fn scalar(ty: Type, value: i64) -> Result<()> {
    if ty == Type::F32 {
        return Err("f32 requires the typed activation executor".into());
    }
    if matches!(ty, Type::Data(_) | Type::Addr | Type::Unit)
        || ty == Type::Bool && !matches!(value, 0 | 1)
        || ty == Type::Event && value != 1
    {
        return Err("unsupported or invalid scalar behavior value".into());
    }
    Ok(())
}
fn assignment(op: Assign, old: Option<i64>, rhs: i64) -> Result<i64> {
    if op == Assign::Set {
        return Ok(rhs);
    }
    arithmetic(
        match op {
            Assign::FAdd => Binary::FAdd,
            Assign::FSub => Binary::FSub,
            Assign::FMul => Binary::FMul,
            Assign::FDiv => Binary::FDiv,
            Assign::Add => Binary::Add,
            Assign::Sub => Binary::Sub,
            Assign::Mul => Binary::Mul,
            Assign::Div => Binary::Div,
            Assign::Rem => Binary::Rem,
            Assign::Set => unreachable!(),
        },
        old.ok_or("missing assignment binding")?,
        rhs,
    )
}
pub fn arithmetic(op: Binary, a: i64, b: i64) -> Result<i64> {
    if matches!(
        op,
        Binary::FAdd
            | Binary::FSub
            | Binary::FMul
            | Binary::FDiv
            | Binary::FEq
            | Binary::FNe
            | Binary::FLt
            | Binary::FLe
            | Binary::FGt
            | Binary::FGe
    ) {
        let a = word_f32(a)?;
        let b = word_f32(b)?;
        return match op {
            Binary::FAdd => f32_word(a + b),
            Binary::FSub => f32_word(a - b),
            Binary::FMul => f32_word(a * b),
            Binary::FDiv => {
                if b == 0.0 {
                    return Err("f32 division by zero".into());
                }
                f32_word(a / b)
            }
            Binary::FEq => Ok(i64::from(a == b)),
            Binary::FNe => Ok(i64::from(a != b)),
            Binary::FLt => Ok(i64::from(a < b)),
            Binary::FLe => Ok(i64::from(a <= b)),
            Binary::FGt => Ok(i64::from(a > b)),
            Binary::FGe => Ok(i64::from(a >= b)),
            _ => unreachable!(),
        };
    }
    Ok(match op {
        Binary::Add => a.checked_add(b).ok_or("i64 overflow")?,
        Binary::Sub => a.checked_sub(b).ok_or("i64 overflow")?,
        Binary::Mul => a.checked_mul(b).ok_or("i64 overflow")?,
        Binary::Div | Binary::Rem => {
            if b == 0 {
                return Err("division by zero".into());
            }
            if op == Binary::Div {
                a.checked_div(b).ok_or("i64 overflow")?
            } else {
                a.checked_rem(b).ok_or("i64 overflow")?
            }
        }
        Binary::Eq => i64::from(a == b),
        Binary::Ne => i64::from(a != b),
        Binary::Lt => i64::from(a < b),
        Binary::Le => i64::from(a <= b),
        Binary::Gt => i64::from(a > b),
        Binary::Ge => i64::from(a >= b),
        Binary::FAdd
        | Binary::FSub
        | Binary::FMul
        | Binary::FDiv
        | Binary::FEq
        | Binary::FNe
        | Binary::FLt
        | Binary::FLe
        | Binary::FGt
        | Binary::FGe => unreachable!(),
        Binary::And => i64::from(a != 0 && b != 0),
        Binary::Or => i64::from(a != 0 || b != 0),
        Binary::BitAnd => a & b,
        Binary::BitOr => a | b,
        Binary::BitXor => a ^ b,
        Binary::Shl | Binary::Shr => {
            if !(0..64).contains(&b) {
                return Err("shift count must be in 0..63".into());
            }
            if op == Binary::Shl {
                ((a as u64) << b) as i64
            } else {
                ((a as u64) >> b) as i64
            }
        }
    })
}

pub fn f32_word(value: f32) -> Result<i64> {
    if !value.is_finite() {
        return Err("nonfinite f32 value".into());
    }
    Ok(value.to_bits() as i64)
}
pub fn word_f32(value: i64) -> Result<f32> {
    let bits = u32::try_from(value).map_err(|_| "invalid f32 representation")?;
    let value = f32::from_bits(bits);
    if !value.is_finite() {
        return Err("nonfinite f32 value".into());
    }
    Ok(value)
}

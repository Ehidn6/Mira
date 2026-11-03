//! Compile-time forward differentiation of pure scalar Mira definitions.
//! Derivatives are ordinary Mira IR, usable by hosted and native execution.
use mira_core::{code::*, *};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Dual {
    value: Expr,
    tangent: Expr,
    ty: Type,
}
fn number(value: f32) -> Expr {
    Expr::Float(value.to_bits())
}
fn binary(op: Binary, a: Expr, b: Expr) -> Expr {
    Expr::Binary(op, Box::new(a), Box::new(b))
}
fn negate(value: Expr) -> Expr {
    Expr::Unary(Unary::FNeg, Box::new(value))
}
fn check_size(value: &Expr, tangent: &Expr) -> Result<()> {
    let mut pending = vec![(value, 0usize), (tangent, 0usize)];
    let mut budget = 20_000usize;
    while let Some((expr, depth)) = pending.pop() {
        budget = budget
            .checked_sub(1)
            .ok_or("AD: expanded derivative exceeds expression budget")?;
        if depth >= 256 {
            return Err("AD: expanded derivative exceeds expression depth budget".into());
        }
        match expr {
            Expr::Unary(_, a) => pending.push((a, depth + 1)),
            Expr::Binary(_, a, b) => {
                pending.push((a, depth + 1));
                pending.push((b, depth + 1));
            }
            Expr::Choose(c, a, b) => {
                pending.push((c, depth + 1));
                pending.push((a, depth + 1));
                pending.push((b, depth + 1));
            }
            Expr::Call(_, args) => pending.extend(args.iter().map(|a| (a, depth + 1))),
            _ => {}
        }
    }
    Ok(())
}
struct Builder<'a> {
    program: &'a Program,
    active: BTreeSet<Id>,
    remaining: usize,
}
impl Builder<'_> {
    fn spend(&mut self) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or("AD: expression expansion budget exceeded")?;
        Ok(())
    }
    fn resolve(&self, owner: Id, name: &str) -> Result<&CodeModule> {
        mira_core::code::resolve(&self.program.code_modules, owner, name)
            .ok_or_else(|| format!("AD: pure helper {name} is not defined"))
    }
    fn module(&mut self, module: &CodeModule, arg: Dual) -> Result<Dual> {
        if module.parameters.len() != 1
            || module.parameters[0].ty != Type::F32
            || module.output != Type::F32
            || module.output_ports
        {
            return Err(format!(
                "AD: {} must have one f32 input and an ordinary f32 result",
                module.name
            ));
        }
        if self.active.len() >= MAX_DEPTH || !self.active.insert(module.id) {
            return Err(format!(
                "AD: recursive or excessively nested helper {}",
                module.name
            ));
        }
        let mut env = BTreeMap::new();
        env.insert(module.parameters[0].name.clone(), arg);
        let result = self.body(module, &mut env);
        self.active.remove(&module.id);
        result.map_err(|error| format!("{}: {error}", module.name))
    }
    fn body(&mut self, module: &CodeModule, env: &mut BTreeMap<String, Dual>) -> Result<Dual> {
        let mut result = None;
        for statement in &module.body {
            self.spend()?;
            match statement {
                Statement::Expr(expr)=> result=Some(self.expr(module,expr,env)?),
                Statement::Bind{name,ty,immutable:_,op,value}=> {
                    let value=self.expr(module,value,env)?;
                    let value=if *op==Assign::Set {value} else {
                        let old=env.get(name).cloned().ok_or_else(||format!("AD: {name}: assignment has no previous value"))?;
                        let operation=match op {
                            Assign::FAdd|Assign::Add if old.ty==Type::F32=>Binary::FAdd,Assign::FSub|Assign::Sub if old.ty==Type::F32=>Binary::FSub,
                            Assign::FMul|Assign::Mul if old.ty==Type::F32=>Binary::FMul,Assign::FDiv|Assign::Div if old.ty==Type::F32=>Binary::FDiv,
                            _=>return Err("AD: only f32 compound assignments are differentiable".into()),
                        };
                        self.arithmetic(operation,old,value)?
                    };
                    if ty.is_some_and(|ty|ty!=value.ty) {return Err(format!("AD: {name}: binding type mismatch"));}
                    check_size(&value.value,&value.tangent)?;
                    env.insert(name.clone(),value);
                    result=None;
                }
                Statement::Set{..}=>return Err("AD: aggregate mutation is not a pure scalar operation".into()),
                Statement::If{..}|Statement::For{..}|Statement::While{..}=>return Err("AD: statement branches and loops are not supported yet; use a scalar conditional expression for pathwise differentiation".into()),
            }
        }
        let result = result.ok_or("AD: definition must end in a scalar result expression")?;
        if result.ty != Type::F32 {
            return Err("AD: result must be f32".into());
        }
        Ok(result)
    }
    fn arithmetic(&mut self, op: Binary, a: Dual, b: Dual) -> Result<Dual> {
        use Binary::*;
        // Source inference may not yet have selected floating opcodes when a
        // derivative request is expanded. Operand types decide the same choice.
        let op = if a.ty == Type::F32 && b.ty == Type::F32 {
            match op {
                Add => FAdd,
                Sub => FSub,
                Mul => FMul,
                Div => FDiv,
                Eq => FEq,
                Ne => FNe,
                Lt => FLt,
                Le => FLe,
                Gt => FGt,
                Ge => FGe,
                other => other,
            }
        } else {
            op
        };
        let float = matches!(
            op,
            FAdd | FSub | FMul | FDiv | FEq | FNe | FLt | FLe | FGt | FGe
        );
        if float && (a.ty != Type::F32 || b.ty != Type::F32) {
            return Err("AD: f32 operation requires two f32 operands".into());
        }
        let tangent = match op {
            FAdd | FSub => binary(op, a.tangent, b.tangent),
            FMul => binary(
                FAdd,
                binary(FMul, a.tangent, b.value.clone()),
                binary(FMul, a.value.clone(), b.tangent),
            ),
            FDiv => binary(
                FDiv,
                binary(
                    FSub,
                    binary(FMul, a.tangent, b.value.clone()),
                    binary(FMul, a.value.clone(), b.tangent),
                ),
                binary(FMul, b.value.clone(), b.value.clone()),
            ),
            FEq | FNe | FLt | FLe | FGt | FGe | Eq | Ne | Lt | Le | Gt | Ge | And | Or => {
                number(0.0)
            }
            _ => {
                return Err(
                    "AD: unsupported arithmetic; convert constants to f32 explicitly".into(),
                );
            }
        };
        let ty = if matches!(op, FAdd | FSub | FMul | FDiv) {
            Type::F32
        } else {
            Type::Bool
        };
        Ok(Dual {
            value: binary(op, a.value, b.value),
            tangent,
            ty,
        })
    }
    fn expr(
        &mut self,
        module: &CodeModule,
        expr: &Expr,
        env: &BTreeMap<String, Dual>,
    ) -> Result<Dual> {
        self.spend()?;
        let result=match expr {
            Expr::Float(value)=>Ok(Dual{value:Expr::Float(*value),tangent:number(0.0),ty:Type::F32}),
            Expr::Int(value)=>Ok(Dual{value:Expr::Int(*value),tangent:number(0.0),ty:Type::I64}),
            Expr::Bool(value)=>Ok(Dual{value:Expr::Bool(*value),tangent:number(0.0),ty:Type::Bool}),
            Expr::Name(name)=>env.get(name).cloned().ok_or_else(||format!("AD: {name}: unknown scalar binding")),
            Expr::Unary(op,value)=> {
                let value=self.expr(module,value,env)?;
                match op {
                    Unary::FNeg | Unary::Neg if value.ty==Type::F32=>Ok(Dual{value:negate(value.value),tangent:negate(value.tangent),ty:Type::F32}),
                    Unary::Plus=>Ok(value),
                    Unary::Not if value.ty==Type::Bool=>Ok(Dual{value:Expr::Unary(*op,Box::new(value.value)),tangent:number(0.0),ty:Type::Bool}),
                    _=>Err("AD: unsupported unary scalar operation".into()),
                }
            }
            Expr::Binary(op,a,b)=> {
                let a=self.expr(module,a,env)?;
                let b=self.expr(module,b,env)?;
                self.arithmetic(*op,a,b)
            }
            Expr::Choose(condition,yes,no)=> {
                let condition=self.expr(module,condition,env)?;
                let yes=self.expr(module,yes,env)?;
                let no=self.expr(module,no,env)?;
                if condition.ty!=Type::Bool || yes.ty!=no.ty {return Err("AD: conditional requires bool and matching result types".into());}
                Ok(Dual{value:Expr::Choose(Box::new(condition.value.clone()),Box::new(yes.value),Box::new(no.value)),tangent:Expr::Choose(Box::new(condition.value),Box::new(yes.tangent),Box::new(no.tangent)),ty:yes.ty})
            }
            Expr::Call(name,args)=> {
                if name.starts_with("sys.") || name.starts_with("cells.") || name.starts_with("graph.") || name.starts_with('#') || name.starts_with("diff.") {
                    return Err(format!("AD: {name}: state, hardware, contextual and nested differentiation effects are forbidden"));
                }
                let [arg]=args.as_slice() else {return Err(format!("AD: {name}: only unary pure functions are supported"));};
                let arg=self.expr(module,arg,env)?;
                if name=="math.sqrt" {
                    if arg.ty!=Type::F32 {return Err("AD: sqrt input must be f32".into());}
                    let value=Expr::Call(name.clone(),vec![arg.value]);
                    return Ok(Dual{tangent:binary(Binary::FDiv,arg.tangent,binary(Binary::FMul,number(2.0),value.clone())),value,ty:Type::F32});
                }
                if name=="math.to_f32" && arg.ty==Type::I64 {
                    return Ok(Dual{value:Expr::Call(name.clone(),vec![arg.value]),tangent:number(0.0),ty:Type::F32});
                }
                let helper=self.resolve(module.id,name)?.clone();
                self.module(&helper,arg)
            }
            _=>Err("AD: only pure scalar expressions can be differentiated; aggregate/address access is unsupported".into()),
        };
        let result = result?;
        check_size(&result.value, &result.tangent)?;
        Ok(result)
    }
}
/// Generate d(target)/dx as ordinary scalar IR. Conditional derivatives follow
/// the selected branch; no derivative is claimed at branch boundaries or sqrt(0).
/// Unsupported constructs are rejected rather than approximated numerically.
pub fn differentiate(
    program: &Program,
    target: Id,
    generated_id: Id,
    label: String,
) -> Result<CodeModule> {
    fn find(modules: &[CodeModule], id: Id) -> Option<&CodeModule> {
        modules.iter().find_map(|m| {
            if m.id == id {
                Some(m)
            } else {
                find(&m.locals, id)
            }
        })
    }
    let module = find(&program.code_modules, target).ok_or("AD: target definition missing")?;
    if generated_id < program.next_identity || !name_ok(&label) {
        return Err("AD: generated definition needs a fresh identity and valid label".into());
    }
    if module.compile_time {
        return Err("AD: a compile-time macro is not a differentiable runtime function".into());
    }
    let input = module
        .parameters
        .first()
        .ok_or("AD: target needs one f32 input")?;
    let mut builder = Builder {
        program,
        active: BTreeSet::new(),
        remaining: 20_000,
    };
    let dual = builder.module(
        module,
        Dual {
            value: Expr::Name(input.name.clone()),
            tangent: number(1.0),
            ty: Type::F32,
        },
    )?;
    check_size(&dual.value, &dual.tangent)?;
    Ok(CodeModule {
        id: generated_id,
        name: label,
        parameters: module.parameters.clone(),
        output: Type::F32,
        output_ports: false,
        compile_time: false,
        annotated: true,
        // Evaluate the original pure function once as well, so a discarded
        // binding with an invalid operation cannot disappear from the domain.
        body: vec![
            Statement::Bind {
                name: "#ad.primal".into(),
                ty: Some(Type::F32),
                immutable: true,
                op: Assign::Set,
                value: Expr::Call(mira_core::code::path(&program.code_modules, module.id).ok_or("AD: target path missing")?, vec![Expr::Name(input.name.clone())]),
            },
            Statement::Expr(dual.tangent),
        ],
        locals: vec![],
        data_types: vec![],
    })
}

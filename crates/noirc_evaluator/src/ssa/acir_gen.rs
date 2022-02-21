use super::node::{Instruction, Operation};
use acvm::acir::OPCODE;
use acvm::FieldElement;
use arena::Index;
use num_traits::{One, Zero};
use std::cmp::Ordering;
use std::collections::HashMap;
//use crate::acir::native_types::{Arithmetic, Witness};
use crate::ssa::{code_gen::IRGenerator, mem, node, node::Node};
use crate::Evaluator;
use crate::Gate;
use crate::RuntimeErrorKind;
use acvm::acir::circuit::gate::{Directive, GadgetCall, GadgetInput};
use acvm::acir::native_types::{Arithmetic, Linear, Witness};
use num_bigint::BigUint;
use std::convert::TryInto;
pub struct Acir {
    pub arith_cache: HashMap<Index, InternalVar>,
    pub memory_map: HashMap<u32, InternalVar>, //maps memory adress to expression
}

#[derive(Clone, Debug)]
pub struct InternalVar {
    expression: Arithmetic,
    //value: FieldElement,     //not used for now
    witness: Option<Witness>,
    idx: Option<Index>,
}
impl InternalVar {
    pub fn is_equal(&self, b: &InternalVar) -> bool {
        (self.idx.is_some() && self.idx == b.idx)
            || (self.witness.is_some() && self.witness == b.witness)
            || self.expression == b.expression
    }

    fn new(expression: Arithmetic, witness: Option<Witness>, id: Index) -> InternalVar {
        InternalVar {
            expression,
            witness,
            idx: Some(id),
        }
    }

    pub fn to_const(&self) -> Option<FieldElement> {
        if self.expression.mul_terms.is_empty() && self.expression.linear_combinations.is_empty() {
            return Some(self.expression.q_c);
        }
        None
    }
}

impl Acir {
    //This function stores the substitution with the arithmetic expression in the cache
    //When an instruction performs arithmetic operation, its output can be represented as an arithmetic expression of its arguments
    //Substitute a nodeobj as an arithmetic expression
    fn substitute(
        &mut self,
        idx: Index,
        evaluator: &mut Evaluator,
        cfg: &IRGenerator,
    ) -> InternalVar {
        if self.arith_cache.contains_key(&idx) {
            return self.arith_cache[&idx].clone();
        }
        let var = match cfg.get_object(idx) {
            Some(node::NodeObj::Const(c)) => {
                let f_value = FieldElement::from_be_bytes_reduce(&c.value.to_bytes_be()); //TODO const should be a field
                let expr = Arithmetic {
                    mul_terms: Vec::new(),
                    linear_combinations: Vec::new(),
                    q_c: f_value, //TODO handle other types
                };
                InternalVar::new(expr, None, idx)
            }
            Some(node::NodeObj::Obj(v)) => {
                let w = if let Some(w1) = v.witness {
                    w1
                } else {
                    evaluator.add_witness_to_cs()
                };
                let expr = Arithmetic::from(&w);
                InternalVar::new(expr, Some(w), idx)
            }
            _ => {
                let w = evaluator.add_witness_to_cs();
                let expr = Arithmetic::from(&w);
                InternalVar::new(expr, Some(w), idx)
            }
        };
        self.arith_cache.insert(idx, var);
        self.arith_cache[&idx].clone()
    }

    pub fn new() -> Acir {
        Acir {
            arith_cache: HashMap::new(),
            memory_map: HashMap::new(),
        }
    }

    pub fn evaluate_instruction(
        &mut self,
        ins: &Instruction,
        evaluator: &mut Evaluator,
        cfg: &IRGenerator,
    ) {
        if ins.operator == Operation::Nop {
            return;
        }
        let l_c = self.substitute(ins.lhs, evaluator, cfg);
        let r_c = self.substitute(ins.rhs, evaluator, cfg);
        let output = match ins.operator {
            Operation::Add | Operation::SafeAdd => {
                add(&l_c.expression, FieldElement::one(), &r_c.expression)
            }
            Operation::Sub | Operation::SafeSub => {
                //we need the type of rhs and its max value, then:
                //lhs-rhs+k*2^bit_size where k=ceil(max_value/2^bit_size)
                let bit_size = cfg.get_object(ins.rhs).unwrap().bits();
                let r_big = BigUint::one() << bit_size;
                let mut k = &ins.max_value / &r_big;
                if &ins.max_value % &r_big != BigUint::zero() {
                    k = &k + BigUint::one();
                }
                k = &k * r_big;
                let f = FieldElement::from_be_bytes_reduce(&k.to_bytes_be());
                let mut output = add(
                    &l_c.expression,
                    FieldElement::from(-1_i128),
                    &r_c.expression,
                );
                output.q_c += f;
                output
            }
            Operation::Mul | Operation::SafeMul => evaluate_mul(&l_c, &r_c, evaluator),
            Operation::Udiv => {
                let (q_wit, _) = evaluate_udiv(&l_c, &r_c, evaluator);
                Arithmetic::from(Linear::from_witness(q_wit))
            }
            Operation::Sdiv => evaluate_sdiv(&l_c, &r_c, evaluator).0,
            Operation::Urem => {
                let (_, r_wit) = evaluate_udiv(&l_c, &r_c, evaluator);
                Arithmetic::from(Linear::from_witness(r_wit))
            }
            Operation::Srem => evaluate_sdiv(&l_c, &r_c, evaluator).1,
            Operation::Div => todo!(),
            Operation::Eq => todo!(),
            Operation::Ne => todo!(),
            Operation::Ugt => todo!(),
            Operation::Uge => todo!(),
            Operation::Ult => todo!(),
            Operation::Ule => todo!(),
            Operation::Sgt => todo!(),
            Operation::Sge => todo!(),
            Operation::Slt => todo!(),
            Operation::Sle => todo!(),
            Operation::Lt => todo!(), //TODO need a quad_decomposition gate from barretenberg
            Operation::Gt => todo!(),
            Operation::Lte => todo!(),
            Operation::Gte => todo!(),
            Operation::And => evaluate_and(l_c, r_c, ins.res_type.bits(), evaluator),
            Operation::Not => todo!(),
            Operation::Or => todo!(),
            Operation::Xor => evaluate_xor(l_c, r_c, ins.res_type.bits(), evaluator),
            Operation::Cast => l_c.expression,
            Operation::Ass | Operation::Jne | Operation::Jeq | Operation::Jmp | Operation::Phi => {
                todo!("invalid instruction");
            }
            Operation::Trunc => {
                assert!(is_const(&r_c.expression));
                evaluate_truncate(
                    l_c,
                    r_c.expression.q_c.to_u128().try_into().unwrap(),
                    ins.bit_size,
                    evaluator,
                )
            }
            Operation::StdLib(opcode) => evaluate_opcode(l_c, opcode, cfg, evaluator),
            Operation::Nop => Arithmetic::default(),
            Operation::EqGate => {
                let output = add(
                    &l_c.expression,
                    FieldElement::from(-1_i128),
                    &r_c.expression,
                );
                evaluator.gates.push(Gate::Arithmetic(output.clone())); //TODO should we create a witness??
                output
            }
            Operation::Load(array_idx) => {
                //retrieves the value from the map if address is known at compile time:
                //address = l_c and should be constant
                if let Some(val) = l_c.to_const() {
                    let address = mem::Memory::as_u32(val);
                    if self.memory_map.contains_key(&address) {
                        self.memory_map[&address].expression.clone()
                    } else {
                        //if not found, then it must be a witness (else it is non-initialised memory)
                        let array = &cfg.mem.arrays[array_idx as usize];
                        let index = (address - array.adr) as usize;
                        let w = array.witness[index];
                        Arithmetic::from(Linear::from_witness(w))
                    }
                } else {
                    todo!();
                }
            }

            Operation::Store(_) => {
                //maps the address to the rhs if address is known at compile time
                if let Some(val) = r_c.to_const() {
                    let address = mem::Memory::as_u32(val);
                    self.memory_map.insert(address, l_c);
                    //we do not generate constraint, so no output.
                    Arithmetic::default()
                } else {
                    todo!();
                }
            }
        };

        let output_var = InternalVar {
            expression: output,
            //value: FieldElement::from(0_u32),
            idx: Some(ins.idx),
            witness: None, //TODO put the witness when it exist
        };

        self.arith_cache.insert(ins.idx, output_var);
    }

    pub fn print_field(f: FieldElement) -> String {
        if f == -FieldElement::one() {
            return "-".to_string();
        }
        if f == FieldElement::one() {
            return String::new();
        }
        let big_f = BigUint::from_bytes_be(&f.to_bytes());

        if big_f.to_string() == "4294967296" {
            return "2^32".to_string();
        }
        let s = big_f.bits();
        let big_s = BigUint::one() << s;
        if big_s == big_f {
            return format!("2^{}", s.to_string());
        }
        if big_f.clone() == BigUint::zero() {
            return "0".to_string();
        }
        if big_f.clone() % BigUint::from(2_u128).pow(32) == BigUint::zero() {
            return format!("2^32*{}", big_f.clone() / BigUint::from(2_u128).pow(32));
        }
        let big_minus = BigUint::from_bytes_be(&(-f).to_bytes());
        if big_minus.to_string().len() < big_f.to_string().len() {
            return format!("-{}", big_minus);
        }
        big_f.to_string()
    }

    pub fn print_gate(g: &Gate) -> String {
        let mut result = String::new();
        match g {
            Gate::Arithmetic(a) => {
                for i in &a.mul_terms {
                    result += &format!(
                        "{}x{}*x{} + ",
                        Acir::print_field(i.0),
                        i.1.witness_index(),
                        i.2.witness_index()
                    );
                }
                for i in &a.linear_combinations {
                    result += &format!("{}x{} + ", Acir::print_field(i.0), i.1.witness_index());
                }
                result += &format!("{} = 0", Acir::print_field(a.q_c));
            }
            Gate::Range(w, s) => {
                result = format!("x{} is {} bits", w.witness_index(), s);
            }
            _ => {
                //dbg!(&g);
            }
        }

        result
    }
}
pub fn evaluate_opcode(
    lhs: InternalVar,
    opcode: OPCODE,
    cfg: &IRGenerator,
    evaluator: &mut Evaluator,
) -> Arithmetic {
    match opcode {
        OPCODE::SHA256 => std_lib_sha256(lhs, cfg, evaluator),
        // OPCODE::MerkleMembership => MerkleMembershipGadget::call(evaluator, env, call_expr),
        // OPCODE::SchnorrVerify => SchnorrVerifyGadget::call(evaluator, env, call_expr),
        // OPCODE::Blake2s => Blake2sGadget::call(evaluator, env, call_expr),
        // OPCODE::Pedersen => PedersenGadget::call(evaluator, env, call_expr),
        // OPCODE::EcdsaSecp256k1 => EcdsaSecp256k1Gadget::call(evaluator, env, call_expr),
        // OPCODE::HashToField => HashToFieldGadget::call(evaluator, env, call_expr),
        // OPCODE::FixedBaseScalarMul => FixedBaseScalarMulGadget::call(evaluator, env, call_expr),
        // OPCODE::InsertRegularMerkle => InsertRegularMerkleGadget::call(evaluator, env, call_expr),
        _ => todo!(),
    }
    Arithmetic::default()
}

pub fn prepare_inputs(pointer: Index, cfg: &IRGenerator) -> Vec<GadgetInput> {
    let l_obj = cfg.get_object(pointer).unwrap();
    let mut inputs: Vec<GadgetInput> = Vec::new();
    match l_obj.get_type() {
        node::ObjectType::Pointer(a) => {
            let array = &cfg.mem.arrays[a as usize];
            let num_bits = array.element_type.bits();
            for i in &array.witness {
                inputs.push(GadgetInput {
                    witness: *i,
                    num_bits,
                });
            }
        }
        _ => unreachable!("invalid input"),
    }
    inputs
}

pub fn prepare_outputs(
    pointer: Index,
    output_nb: u32,
    cfg: &IRGenerator,
    evaluator: &mut Evaluator,
) -> Vec<Witness> {
    // Create fresh variables that will link to the output
    let mut outputs = Vec::with_capacity(output_nb as usize);
    for _ in 0..output_nb {
        let witness = evaluator.add_witness_to_cs();
        outputs.push(witness);
    }

    let l_obj = cfg.get_object(pointer).unwrap();
    match l_obj.get_type() {
        // node::ObjectType::Pointer(a) => {
        //     let array = &mut cfg.mem.arrays[a as usize];
        //     array.witness = outputs;
        //}
        _ => unreachable!("invalid output"),
    }
    outputs
}

fn std_lib_blake2s(lhs: InternalVar, cfg: &IRGenerator, evaluator: &mut Evaluator)
//-> Result<Object, RuntimeError>
{
    let inputs = prepare_inputs(lhs.idx.unwrap(), cfg);
    let outputs = Vec::new(); //TODO...prepare_outputs(ins.res_type, 32, cfg, evaluator);
    let sha256_gate = GadgetCall {
        name: OPCODE::Blake2s,
        inputs,  //witness + bit size
        outputs, //witness
    };

    evaluator.gates.push(Gate::GadgetCall(sha256_gate));
}

fn std_lib_sha256(lhs: InternalVar, cfg: &IRGenerator, evaluator: &mut Evaluator)
//-> Result<Object, RuntimeError>
{
    let inputs = prepare_inputs(lhs.idx.unwrap(), cfg);
    let outputs = prepare_outputs(lhs.idx.unwrap(), 32, cfg, evaluator); //TODO pas lhs.idx mais ins.res_type!!!

    let sha256_gate = GadgetCall {
        name: OPCODE::SHA256,
        inputs,  //witness + bit size
        outputs, //witness
    };

    evaluator.gates.push(Gate::GadgetCall(sha256_gate));

    //what to return??

    // Ok(Object::Array(arr))
}

pub fn evaluate_and(
    lhs: InternalVar,
    rhs: InternalVar,
    bit_size: u32,
    evaluator: &mut Evaluator,
) -> Arithmetic {
    let result = evaluator.add_witness_to_cs();
    let a_witness = lhs
        .witness
        .unwrap_or_else(|| generate_witness(&lhs, evaluator));
    let b_witness = rhs
        .witness
        .unwrap_or_else(|| generate_witness(&rhs, evaluator));
    //TODO checks the cost of the gate vs bit_size (cf. #164)
    evaluator
        .gates
        .push(Gate::And(acvm::acir::circuit::gate::AndGate {
            a: a_witness,
            b: b_witness,
            result,
            num_bits: bit_size,
        }));
    Arithmetic::from(Linear::from_witness(result))
}

pub fn evaluate_xor(
    lhs: InternalVar,
    rhs: InternalVar,
    bit_size: u32,
    evaluator: &mut Evaluator,
) -> Arithmetic {
    let result = evaluator.add_witness_to_cs();

    let a_witness = lhs
        .witness
        .unwrap_or_else(|| generate_witness(&lhs, evaluator));
    let b_witness = lhs
        .witness
        .unwrap_or_else(|| generate_witness(&rhs, evaluator));
    //TODO checks the cost of the gate vs bit_size (cf. #164)
    evaluator
        .gates
        .push(Gate::Xor(acvm::acir::circuit::gate::XorGate {
            a: a_witness,
            b: b_witness,
            result,
            num_bits: bit_size,
        }));
    Arithmetic::from(Linear::from_witness(result))
}

//truncate lhs (a number whose value requires max_bits) into a rhs-bits number: i.e it returns b such that lhs mod 2^rhs is b
pub fn evaluate_truncate(
    lhs: InternalVar,
    rhs: u32,
    max_bits: u32,
    evaluator: &mut Evaluator,
) -> Arithmetic {
    // dbg!(&max_bits);
    // dbg!(&rhs);
    assert!(max_bits > rhs);
    //1. Generate witnesses a,b,c
    //TODO: we should truncate the arithmetic expression (and so avoid having to create a witness)
    // if lhs is not a witness, but this requires a new truncate directive...TODO
    let a_witness = lhs
        .witness
        .unwrap_or_else(|| generate_witness(&lhs, evaluator));
    // if lhs.witness.is_none() {
    //     dbg!(a_witness);
    //     dbg!(&lhs.expression);
    // }
    let b_witness = evaluator.add_witness_to_cs();
    let c_witness = evaluator.add_witness_to_cs();
    evaluator.gates.push(Gate::Directive(Directive::Truncate {
        a: a_witness,
        b: b_witness,
        c: c_witness,
        bit_size: rhs,
    }));

    range_constraint(b_witness, rhs, evaluator).unwrap_or_else(|err| {
        dbg!(err);
    }); //TODO propagate the error using ?
    range_constraint(c_witness, max_bits - rhs, evaluator).unwrap_or_else(|err| {
        dbg!(err);
    });

    //2. Add the constraint a = b+2^Nc
    let mut f = FieldElement::from(2_i128);
    f = f.pow(&FieldElement::from(rhs as i128));
    let b_arith = from_witness(b_witness);
    let c_arith = from_witness(c_witness);
    let res = add(&b_arith, f, &c_arith); //b+2^Nc
    let a = &Arithmetic::from(Linear::from_witness(a_witness));
    let my_constraint = add(&res, -FieldElement::one(), a);
    evaluator.gates.push(Gate::Arithmetic(my_constraint));

    Arithmetic::from(Linear::from_witness(b_witness))
}

pub fn generate_witness(lhs: &InternalVar, evaluator: &mut Evaluator) -> Witness {
    if let Some(witness) = lhs.witness {
        return witness;
    }

    if is_const(&lhs.expression) {
        todo!("Panic");
    }
    if lhs.expression.mul_terms.is_empty() && lhs.expression.linear_combinations.len() == 1 {
        //TODO check if this case can be optimised
    }
    let (_, w) = evaluator.create_intermediate_variable(lhs.expression.clone());
    w

    // let w = evaluator.add_witness_to_cs(); //TODO  set lhs.witness = w
    // let (_,w) = evaluator.create_intermediate_variable(&lhs.expression);
    // evaluator
    //     .gates
    //     .push(Gate::Arithmetic(&lhs.expression - &Arithmetic::from(&w)));
    // w
}

pub fn evaluate_mul(lhs: &InternalVar, rhs: &InternalVar, evaluator: &mut Evaluator) -> Arithmetic {
    if is_const(&lhs.expression) {
        return &rhs.expression * &lhs.expression.q_c;
    }
    if is_const(&rhs.expression) {
        return &lhs.expression * &rhs.expression.q_c;
    }
    //No multiplicative term
    if lhs.expression.mul_terms.is_empty() && rhs.expression.mul_terms.is_empty() {
        return mul(&lhs.expression, &rhs.expression);
    }
    //Generate intermediate variable
    //create new witness a and a gate: a = lhs
    let a = evaluator.add_witness_to_cs();
    evaluator
        .gates
        .push(Gate::Arithmetic(&lhs.expression - &Arithmetic::from(&a)));
    //create new witness b and gate b = rhs
    let mut b = a;
    if !lhs.is_equal(rhs) {
        b = evaluator.add_witness_to_cs();
        evaluator
            .gates
            .push(Gate::Arithmetic(&rhs.expression - &Arithmetic::from(&b)));
    }

    //return arith(mul=a*b)
    mul(&Arithmetic::from(&a), &Arithmetic::from(&b)) //TODO  &lhs.expression * &rhs.expression
}

pub fn evaluate_udiv(
    lhs: &InternalVar,
    rhs: &InternalVar,
    evaluator: &mut Evaluator,
) -> (Witness, Witness) {
    //a = q*b+r, a= lhs, et b= rhs
    //result = q
    //n.b a et b MUST have proper bit size
    //we need to know a bit size (so that q has the same)
    //generate witnesses

    //TODO: can we handle an arithmetic and not create a witness for a and b?
    let a_witness = if let Some(lhs_witness) = lhs.witness {
        lhs_witness
    } else {
        generate_witness(lhs, evaluator) //TODO we should set lhs.witness = a.witness and lhs.expression= 1*w
    };

    //TODO: can we handle an arithmetic and not create a witness for a and b?
    let b_witness = if let Some(rhs_witness) = rhs.witness {
        rhs_witness
    } else {
        generate_witness(rhs, evaluator)
    };
    let q_witness = evaluator.add_witness_to_cs();
    let r_witness = evaluator.add_witness_to_cs();

    //TODO not in master...
    evaluator.gates.push(Gate::Directive(Directive::Quotient {
        a: a_witness,
        b: b_witness,
        q: q_witness,
        r: r_witness,
    }));
    //r<b
    let r_expr = Arithmetic::from(Linear::from_witness(r_witness));
    let r_var = InternalVar {
        expression: r_expr,
        witness: Some(r_witness),
        idx: None,
    };
    bound_check(&r_var, rhs, true, 32, evaluator); //TODO bit size! should be max(a.bit, b.bit)
                                                   //range check q<=a
    range_constraint(q_witness, 32, evaluator).unwrap_or_else(|err| {
        dbg!(err);
    }); //todo bit size should be a.bits
        // a-b*q-r = 0
    let div_eucl = add(
        &lhs.expression,
        -FieldElement::one(),
        &Arithmetic {
            mul_terms: vec![(FieldElement::one(), b_witness, q_witness)],
            linear_combinations: vec![(FieldElement::one(), r_witness)],
            q_c: FieldElement::zero(),
        },
    );

    evaluator.gates.push(Gate::Arithmetic(div_eucl));
    (q_witness, r_witness)
}

//TODO: returns the sign bit of lhs
pub fn sign(lhs: &InternalVar, s: u32, evaluator: &mut Evaluator) -> Witness {
    //TODO:
    //we need to bit size s of lhs..we can get this from the res_type of the instruction
    if s % 2 == 0 {
        range_constraint(lhs.witness.unwrap(), s + 2, evaluator); //todo check the s+2
                                                                  //TODO range_constraint should returns the quad decomposition
                                                                  //Then take the last quad and use the 'new' bit-decomposition gate
    }
    return lhs.witness.unwrap(); //..TODO
}
pub fn evaluate_sdiv(
    lhs: &InternalVar,
    rhs: &InternalVar,
    evaluator: &mut Evaluator,
) -> (Arithmetic, Arithmetic) {
    //TODO
    todo!();
    // let last_bit_a_wit = sign(lhs, 32, evaluator);//TODO bit size
    // let last_bit_b_wit = sign(rhs, 32, evaluator);//TODO bit size
    // //sa=1-2la; sa*lhs
    // let sa =& Arithmetic{
    //     mul_terms: Vec::new(),
    //     linear_combinations: vec![(FieldElement::from(-2_i128),last_bit_a_wit)],
    //     q_c: FieldElement::one(),
    // };
    // let sb = &Arithmetic{
    //     mul_terms: Vec::new(),
    //     linear_combinations: vec![(FieldElement::from(-2_i128),last_bit_b_wit)],
    //     q_c: FieldElement::one(),
    // };
    // let (uq_wit, ur_wit) = evaluate_udiv(mul(sa, &lhs.expression), mul(sb, &rhs.expression), evaluator);
    // //result is
    // let r_arith = &Arithmetic::from(Linear::from_witness(ur_wit));
    // let q_arith = &Arithmetic::from(Linear::from_witness(uq_wit));
    // (mul(sb, &mul(sa, q_arith)), mul(sa, r_arith))
}

pub fn is_const(expr: &Arithmetic) -> bool {
    expr.mul_terms.is_empty() && expr.linear_combinations.is_empty()
}

//a*b
pub fn mul(a: &Arithmetic, b: &Arithmetic) -> Arithmetic {
    if !(a.mul_terms.is_empty() && b.mul_terms.is_empty()) {
        todo!("PANIC");
    }

    let mut output = Arithmetic {
        mul_terms: Vec::new(),
        linear_combinations: Vec::new(),
        q_c: FieldElement::zero(),
    };

    //TODO to optimise...
    for lc in &a.linear_combinations {
        let single = single_mul(lc.1, b);
        output = add(&output, lc.0, &single);
    }

    //linear terms
    let mut i1 = 0; //a
    let mut i2 = 0; //b
    while i1 < a.linear_combinations.len() && i2 < b.linear_combinations.len() {
        let coef_a = b.q_c * a.linear_combinations[i1].0;
        let coef_b = a.q_c * b.linear_combinations[i2].0;
        match a.linear_combinations[i1]
            .1
            .cmp(&b.linear_combinations[i2].1)
        {
            Ordering::Greater => {
                if coef_b != FieldElement::zero() {
                    output
                        .linear_combinations
                        .push((coef_b, b.linear_combinations[i2].1));
                }
                if i2 + 1 >= b.linear_combinations.len() {
                    i1 += 1;
                } else {
                    i2 += 1;
                }
            }
            Ordering::Less => {
                if coef_a != FieldElement::zero() {
                    output
                        .linear_combinations
                        .push((coef_a, a.linear_combinations[i1].1));
                }
                if i1 + 1 >= a.linear_combinations.len() {
                    i2 += 1;
                } else {
                    i1 += 1;
                }
            }
            Ordering::Equal => {
                if coef_a + coef_b != FieldElement::zero() {
                    output
                        .linear_combinations
                        .push((coef_a + coef_b, a.linear_combinations[i1].1));
                }
                if (i1 + 1 >= a.linear_combinations.len())
                    && (i2 + 1 >= b.linear_combinations.len())
                {
                    i1 += 1;
                    i2 += 1;
                } else {
                    if i1 + 1 < a.linear_combinations.len() {
                        i1 += 1;
                    }
                    if i2 + 1 < a.linear_combinations.len() {
                        i2 += 1;
                    }
                }
            }
        }
    }
    //Constant term:
    output.q_c = a.q_c * b.q_c;
    output
}

// returns a + k*b
pub fn add(a: &Arithmetic, k: FieldElement, b: &Arithmetic) -> Arithmetic {
    let mut output = Arithmetic::default();

    //linear combinations
    let mut i1 = 0; //a
    let mut i2 = 0; //b
    while i1 < a.linear_combinations.len() && i2 < b.linear_combinations.len() {
        match a.linear_combinations[i1]
            .1
            .cmp(&b.linear_combinations[i2].1)
        {
            Ordering::Greater => {
                let coef = b.linear_combinations[i2].0 * k;
                if coef != FieldElement::zero() {
                    output
                        .linear_combinations
                        .push((coef, b.linear_combinations[i2].1));
                }
                i2 += 1;
            }
            Ordering::Less => {
                output.linear_combinations.push(a.linear_combinations[i1]);
                i1 += 1;
            }
            Ordering::Equal => {
                let coef = a.linear_combinations[i1].0 + b.linear_combinations[i2].0 * k;
                if coef != FieldElement::zero() {
                    output
                        .linear_combinations
                        .push((coef, a.linear_combinations[i1].1));
                }
                i2 += 1;
                i1 += 1;
            }
        }
    }
    while i1 < a.linear_combinations.len() {
        output.linear_combinations.push(a.linear_combinations[i1]);
        i1 += 1;
    }
    while i2 < b.linear_combinations.len() {
        let coef = b.linear_combinations[i2].0 * k;
        if coef != FieldElement::zero() {
            output
                .linear_combinations
                .push((coef, b.linear_combinations[i2].1));
        }
        i2 += 1;
    }

    //mul terms

    i1 = 0; //a
    i2 = 0; //b

    while i1 < a.mul_terms.len() && i2 < b.mul_terms.len() {
        match (a.mul_terms[i1].1, a.mul_terms[i1].2).cmp(&(b.mul_terms[i2].1, b.mul_terms[i2].2)) {
            Ordering::Greater => {
                let coef = b.mul_terms[i2].0 * k;
                if coef != FieldElement::zero() {
                    output
                        .mul_terms
                        .push((coef, b.mul_terms[i2].1, b.mul_terms[i2].2));
                }
                i2 += 1;
            }
            Ordering::Less => {
                output.mul_terms.push(a.mul_terms[i1]);
                i1 += 1;
            }
            Ordering::Equal => {
                let coef = a.mul_terms[i1].0 + b.mul_terms[i2].0 * k;
                if coef != FieldElement::zero() {
                    output
                        .mul_terms
                        .push((coef, a.mul_terms[i1].1, a.mul_terms[i1].2));
                }
                i2 += 1;
                i1 += 1;
            }
        }
    }
    while i1 < a.mul_terms.len() {
        output.mul_terms.push(a.mul_terms[i1]);
        i1 += 1;
    }

    while i2 < b.mul_terms.len() {
        let coef = b.mul_terms[i2].0 * k;
        if coef != FieldElement::zero() {
            output
                .mul_terms
                .push((coef, b.mul_terms[i2].1, b.mul_terms[i2].2));
        }
        i2 += 1;
    }

    output.q_c = a.q_c + k * b.q_c;
    output
}

// returns w*b.linear_combinations
pub fn single_mul(w: Witness, b: &Arithmetic) -> Arithmetic {
    let mut output = Arithmetic::default();
    let mut i1 = 0;
    while i1 < b.linear_combinations.len() {
        if (w, b.linear_combinations[i1].1) < (b.linear_combinations[i1].1, w) {
            output
                .mul_terms
                .push((b.linear_combinations[i1].0, w, b.linear_combinations[i1].1));
        } else {
            output
                .mul_terms
                .push((b.linear_combinations[i1].0, b.linear_combinations[i1].1, w));
        }
        i1 += 1;
    }
    output
}

pub fn boolean(witness: Witness) -> Arithmetic {
    Arithmetic {
        mul_terms: vec![(FieldElement::one(), witness, witness)],
        linear_combinations: vec![(-FieldElement::one(), witness)],
        q_c: FieldElement::zero(),
    }
}

//contrain witness a to be num_bits-size integer, i.e between 0 and 2^num_bits-1
pub fn range_constraint(
    witness: Witness,
    num_bits: u32,
    evaluator: &mut Evaluator,
) -> Result<(), RuntimeErrorKind> {
    if num_bits == 1 {
        // Add a bool gate
        let bool_constraint = boolean(witness);
        evaluator.gates.push(Gate::Arithmetic(bool_constraint));
    } else if num_bits == FieldElement::max_num_bits() {
        // Don't apply any constraints if the range is for the maximum number of bits
        let message = format!(
            "All Witnesses are by default u{}. Applying this type does not apply any constraints.",
            FieldElement::max_num_bits()
        );
        return Err(RuntimeErrorKind::UnstructuredError { message });
    } else if num_bits % 2 == 1 {
        // Note if the number of bits is odd, then Barretenberg will panic
        // new witnesses; r is constrained to num_bits-1 and b is 1 bit
        let r_witness = evaluator.add_witness_to_cs();
        let b_witness = evaluator.add_witness_to_cs();
        evaluator.gates.push(Gate::Directive(Directive::Oddrange {
            a: witness,
            b: b_witness,
            r: r_witness,
            bit_size: num_bits,
        }));
        range_constraint(r_witness, num_bits - 1, evaluator).unwrap_or_else(|err| {
            dbg!(err);
        });
        range_constraint(b_witness, 1, evaluator).unwrap_or_else(|err| {
            dbg!(err);
        });
        //Add the constraint a = r + 2^N*b
        let mut f = FieldElement::from(2_i128);
        f = f.pow(&FieldElement::from((num_bits - 1) as i128));
        let res = add(&from_witness(r_witness), f, &from_witness(b_witness));
        let my_constraint = add(&res, -FieldElement::one(), &from_witness(witness));
        evaluator.gates.push(Gate::Arithmetic(my_constraint));
    } else {
        evaluator.gates.push(Gate::Range(witness, num_bits));
    }

    Ok(())
}

// Generate constraints that are satisfied iff
// a < b , when strict is true, or
// a <= b, when strict is false
// bits is the bit size of a and b (or an upper bound of the bit size)
///////////////
// a<=b is done by constraining b-a to a bit size of 'bits':
// if a<=b, 0 <= b-a <= b < 2^bits
// if a>b, b-a = p+b-a > p-2^bits >= 2^bits  (if log(p) >= bits + 1)
// n.b: we do NOT check here that a and b are indeed 'bits' size
// a < b <=> a+1<=b
fn bound_check(
    a: &InternalVar,
    b: &InternalVar,
    strict: bool,
    bits: u32,
    evaluator: &mut Evaluator,
) {
    //todo appeler bound_constrains et rajouter les gates a l'evaluator
    if bits > FieldElement::max_num_bits() - 1
    //TODO max_num_bits() is not log(p)?
    {
        todo!("ERROR");
    }
    let offset = if strict {
        FieldElement::one()
    } else {
        FieldElement::zero()
    };
    let mut sub_expression = add(&b.expression, -FieldElement::one(), &a.expression); //a-b
    sub_expression.q_c += offset; //a-b+offset
    let w = evaluator.add_witness_to_cs(); //range_check requires a witness - TODO may be this can be avoided?
    evaluator
        .gates
        .push(Gate::Arithmetic(&sub_expression - &Arithmetic::from(&w)));
    range_constraint(w, bits, evaluator).unwrap_or_else(|err| {
        dbg!(err);
    });
}

pub fn from_witness(witness: Witness) -> Arithmetic {
    Arithmetic {
        mul_terms: Vec::new(),
        linear_combinations: vec![(FieldElement::one(), witness)],
        q_c: FieldElement::zero(),
    }
}
use crate::soft_f32::{u32_widen_mul, SoftF32};

type F = SoftF32;

type FInt = u32;

pub(crate) const fn div(a: F, b: F) -> F {
    let one: FInt = 1;
    let zero: FInt = 0;

    // let bits = F::BITS;
    let significand_bits = F::SIGNIFICAND_BITS;
    let max_exponent = F::EXPONENT_MAX;

    let exponent_bias = F::EXPONENT_BIAS;

    let implicit_bit = F::IMPLICIT_BIT;
    let significand_mask = F::SIGNIFICAND_MASK;
    let sign_bit = F::SIGN_MASK as FInt;
    let abs_mask = sign_bit - one;
    let exponent_mask = F::EXPONENT_MASK;
    let inf_rep = exponent_mask;
    let quiet_bit = implicit_bit >> 1;
    let qnan_rep = exponent_mask | quiet_bit;

    #[inline(always)]
    const fn negate_u32(a: u32) -> u32 {
        (<i32>::wrapping_neg(a as i32)) as u32
    }

    let a_rep = a.repr();
    let b_rep = b.repr();

    let a_exponent = (a_rep >> significand_bits) & max_exponent as FInt;
    let b_exponent = (b_rep >> significand_bits) & max_exponent as FInt;
    let quotient_sign = (a_rep ^ b_rep) & sign_bit;

    let mut a_significand = a_rep & significand_mask;
    let mut b_significand = b_rep & significand_mask;
    let mut scale = 0;

    // Detect if a or b is zero, denormal, infinity, or NaN.
    if a_exponent.wrapping_sub(one) >= (max_exponent - 1) as FInt
        || b_exponent.wrapping_sub(one) >= (max_exponent - 1) as FInt
    {
        let a_abs = a_rep & abs_mask;
        let b_abs = b_rep & abs_mask;

        // NaN / anything = qNaN
        if a_abs > inf_rep {
            return F::from_repr(a_rep | quiet_bit);
        }
        // anything / NaN = qNaN
        if b_abs > inf_rep {
            return F::from_repr(b_rep | quiet_bit);
        }

        if a_abs == inf_rep {
            if b_abs == inf_rep {
                // infinity / infinity = NaN
                return F::from_repr(qnan_rep);
            } else {
                // infinity / anything else = +/- infinity
                return F::from_repr(a_abs | quotient_sign);
            }
        }

        // anything else / infinity = +/- 0
        if b_abs == inf_rep {
            return F::from_repr(quotient_sign);
        }

        if a_abs == zero {
            if b_abs == zero {
                // zero / zero = NaN
                return F::from_repr(qnan_rep);
            } else {
                // zero / anything else = +/- zero
                return F::from_repr(quotient_sign);
            }
        }

        // anything else / zero = +/- infinity
        if b_abs == zero {
            return F::from_repr(inf_rep | quotient_sign);
        }

        // one or both of a or b is denormal, the other (if applicable) is a
        // normal number.  Renormalize one or both of a and b, and set scale to
        // include the necessary exponent adjustment.
        if a_abs < implicit_bit {
            let (exponent, significand) = F::normalize(a_significand);
            scale += exponent;
            a_significand = significand;
        }

        if b_abs < implicit_bit {
            let (exponent, significand) = F::normalize(b_significand);
            scale -= exponent;
            b_significand = significand;
        }
    }

    // Or in the implicit significand bit.  (If we fell through from the
    // denormal path it was already set by normalize( ), but setting it twice
    // won't hurt anything.)
    a_significand |= implicit_bit;
    b_significand |= implicit_bit;
    let mut quotient_exponent: i32 = (a_exponent as i32)
        .wrapping_sub(b_exponent as i32)
        .wrapping_add(scale);

    // Align the significand of b as a Q31 fixed-point number in the range
    // [1, 2.0) and get a Q32 approximate reciprocal using a small minimax
    // polynomial approximation: reciprocal = 3/4 + 1/sqrt(2) - b/2.  This
    // is accurate to about 3.5 binary digits.
    let q31b = (b_significand << 8) as u32;
    let mut reciprocal = (0x7504f333u32).wrapping_sub(q31b);

    // Now refine the reciprocal estimate using a Newton-Raphson iteration:
    //
    //     x1 = x0 * (2 - x0 * b)
    //
    // This doubles the number of correct binary digits in the approximation
    // with each iteration, so after three iterations, we have about 28 binary
    // digits of accuracy.

    let mut correction: u32 =
        negate_u32(((reciprocal as u64).wrapping_mul(q31b as u64) >> 32) as u32);
    reciprocal = ((reciprocal as u64).wrapping_mul(correction as u64) >> 31) as u32;
    correction = negate_u32(((reciprocal as u64).wrapping_mul(q31b as u64) >> 32) as u32);
    reciprocal = ((reciprocal as u64).wrapping_mul(correction as u64) >> 31) as u32;
    correction = negate_u32(((reciprocal as u64).wrapping_mul(q31b as u64) >> 32) as u32);
    reciprocal = ((reciprocal as u64).wrapping_mul(correction as u64) >> 31) as u32;

    // Exhaustive testing shows that the error in reciprocal after three steps
    // is in the interval [-0x1.f58108p-31, 0x1.d0e48cp-29], in line with our
    // expectations.  We bump the reciprocal by a tiny value to force the error
    // to be strictly positive (in the range [0x1.4fdfp-37,0x1.287246p-29], to
    // be specific).  This also causes 1/1 to give a sensible approximation
    // instead of zero (due to overflow).
    reciprocal = reciprocal.wrapping_sub(2);

    // The numerical reciprocal is accurate to within 2^-28, lies in the
    // interval [0x1.000000eep-1, 0x1.fffffffcp-1], and is strictly smaller
    // than the true reciprocal of b.  Multiplying a by this reciprocal thus
    // gives a numerical q = a/b in Q24 with the following properties:
    //
    //    1. q < a/b
    //    2. q is in the interval [0x1.000000eep-1, 0x1.fffffffcp0)
    //    3. the error in q is at most 2^-24 + 2^-27 -- the 2^24 term comes
    //       from the fact that we truncate the product, and the 2^27 term
    //       is the error in the reciprocal of b scaled by the maximum
    //       possible value of a.  As a consequence of this error bound,
    //       either q or nextafter(q) is the correctly rounded
    let mut quotient = u32_widen_mul(a_significand << 1, reciprocal as FInt).1;

    // Two cases: quotient is in [0.5, 1.0) or quotient is in [1.0, 2.0).
    // In either case, we are going to compute a residual of the form
    //
    //     r = a - q*b
    //
    // We know from the construction of q that r satisfies:
    //
    //     0 <= r < ulp(q)*b
    //
    // if r is greater than 1/2 ulp(q)*b, then q rounds up.  Otherwise, we
    // already have the correct result.  The exact halfway case cannot occur.
    // We also take this time to right shift quotient if it falls in the [1,2)
    // range and adjust the exponent accordingly.
    let residual = if quotient < (implicit_bit << 1) {
        quotient_exponent = quotient_exponent.wrapping_sub(1);
        (a_significand << (significand_bits + 1)).wrapping_sub(quotient.wrapping_mul(b_significand))
    } else {
        quotient >>= 1;
        (a_significand << significand_bits).wrapping_sub(quotient.wrapping_mul(b_significand))
    };

    let written_exponent = quotient_exponent.wrapping_add(exponent_bias as i32);

    if written_exponent >= max_exponent as i32 {
        // If we have overflowed the exponent, return infinity.
        return F::from_repr(inf_rep | quotient_sign);
    } else if written_exponent < 1 {
        // Flush denormals to zero.  In the future, it would be nice to add
        // code to round them correctly.
        return F::from_repr(quotient_sign);
    } else {
        let round = ((residual << 1) > b_significand) as u32;
        // Clear the implicit bits
        let mut abs_result = quotient & significand_mask;
        // Insert the exponent
        abs_result |= (written_exponent as FInt) << significand_bits;
        // Round
        abs_result = abs_result.wrapping_add(round as FInt);
        // Insert the sign and return
        return F::from_repr(abs_result | quotient_sign);
    }
}

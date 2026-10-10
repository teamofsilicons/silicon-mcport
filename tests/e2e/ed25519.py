"""Ed25519 signatures (RFC 8032, section 5.1) in pure Python, for test fixtures only.

The fake Silicon Accounts (accounts_fake.py) signs access tokens with it so the end-to-end journey needs nothing
beyond the standard library. It is slow and not constant time: never use it outside tests.
"""
import hashlib

P = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493  # the group order
D = -121665 * pow(121666, P - 2, P) % P
SQRT_M1 = pow(2, (P - 1) // 4, P)


def _recover_x(y, sign):
    x2 = (y * y - 1) * pow(D * y * y + 1, P - 2, P) % P
    if x2 == 0:
        return 0
    x = pow(x2, (P + 3) // 8, P)
    if (x * x - x2) % P:
        x = x * SQRT_M1 % P
    if (x * x - x2) % P:
        raise ValueError("not a curve point")
    return P - x if (x & 1) != sign else x


_GY = 4 * pow(5, P - 2, P) % P
_GX = _recover_x(_GY, 0)
BASE = (_GX, _GY, 1, _GX * _GY % P)  # extended homogeneous coordinates (X, Y, Z, T)


def _add(p, q):
    a = (p[1] - p[0]) * (q[1] - q[0]) % P
    b = (p[1] + p[0]) * (q[1] + q[0]) % P
    c = 2 * p[3] * q[3] * D % P
    d = 2 * p[2] * q[2] % P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % P, g * h % P, f * g % P, e * h % P)


def _mul(scalar, point):
    result = (0, 1, 1, 0)
    while scalar:
        if scalar & 1:
            result = _add(result, point)
        point = _add(point, point)
        scalar >>= 1
    return result


def _compress(point):
    z = pow(point[2], P - 2, P)
    x, y = point[0] * z % P, point[1] * z % P
    return (y | (x & 1) << 255).to_bytes(32, "little")


def _expand(seed):
    if len(seed) != 32:
        raise ValueError("an Ed25519 private key is 32 bytes")
    digest = hashlib.sha512(seed).digest()
    scalar = int.from_bytes(digest[:32], "little")
    scalar &= (1 << 254) - 8
    scalar |= 1 << 254
    return scalar, digest[32:]


def public_key(seed):
    """The 32-byte public key of a 32-byte private key (seed)."""
    scalar, _ = _expand(seed)
    return _compress(_mul(scalar, BASE))


def sign(seed, message, public=None):
    """The 64-byte signature of `message`; pass `public` to skip recomputing the public key."""
    scalar, prefix = _expand(seed)
    public = public or _compress(_mul(scalar, BASE))
    r = int.from_bytes(hashlib.sha512(prefix + message).digest(), "little") % L
    encoded_r = _compress(_mul(r, BASE))
    h = int.from_bytes(hashlib.sha512(encoded_r + public + message).digest(), "little") % L
    return encoded_r + ((r + h * scalar) % L).to_bytes(32, "little")

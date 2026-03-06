# Post-Quantum Cryptography in ac-client

## Overview

The ac-client (OpenWrt package) includes **post-quantum cryptography by default** using the `rustls-post-quantum` crate. This provides hybrid post-quantum key exchange (X25519 + ML-KEM-768/Kyber) to protect against quantum computer attacks.

## Status

✅ **Post-quantum crypto is ALWAYS ENABLED** - There is no configuration toggle. The client automatically uses post-quantum key exchange when connecting to TLS 1.3 servers.

## Technical Details

- **Library**: rustls-post-quantum v0.2
- **Algorithm**: X25519Kyber768 (hybrid)
  - Combines classical X25519 ECDH
  - With ML-KEM-768 (Kyber) post-quantum KEM
- **Protocol**: TLS 1.3
- **Cipher**: TLS13_AES_256_GCM_SHA384

## Verification

When the ac-client starts, you should see:
```
[INFO ac_client] ac-client starting (MTP=WebSocket)
```

If post-quantum initialization fails, the client will exit with:
```
[ERROR ac_client] FATAL: post-quantum TLS provider failed to initialise
```

## Compatibility

- **ac-server**: Fully compatible with post-quantum TLS when `USE_POST_QUANTUM_TLS=true` is set
- **Step-ca**: Certificates work with both post-quantum and standard TLS
- **OpenWrt Targets**: Supported on all architectures (x86_64, ARM64, ARMv7, MIPS)

## Build Requirements

The post-quantum crypto is compiled in by default. No special flags needed:
```bash
cargo build --release
```

## Runtime Verification

To verify post-quantum is working:
1. Check that ac-client connects successfully to server
2. Look for TLS 1.3 handshake in logs
3. Connection should succeed without "post-quantum TLS provider failed" error

The absence of error messages indicates successful post-quantum operation.

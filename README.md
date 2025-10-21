# Bifrost

A WebTransport proxy for sending Solana transactions directly to validator TPUs from browsers.

## What's This?

Bifrost lets browsers send txs directly to Solana validators without RPCs. 

## Quick Start

### 1. Generate Certificates

```bash
# Make a folder for certs
mkdir certs

# Generate a self-signed certificate (valid for 10 days)
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
  -days 10 -nodes -keyout certs/key.pem -out certs/cert.pem \
  -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"
```

Yeah, it's a long command. Just copy-paste it and you're good.

### 2. Start a Local Validator

Fire up a test validator in a separate terminal:
```bash
solana-test-validator
```

Let it run. You'll see some logs scroll by - that's normal.

### 3. Run Bifrost

In another terminal:
```bash
cargo run

or the way I prefer to run it

RUST_LOG=info cargo run
```

You should see:
```
[INFO] Starting Bifrost on [::]:4433
[INFO] Loaded 1 certificate(s) and private key
```

Cool, Bifrost is listening.

### 4. Test It

You've got two options:

**Option A: Run the Rust example**
```bash
cargo run --example client
```

**Option B: Test from your browser**

Just open `test.html` in your browser (Chrome/Edge work best).

Click "Test Connection" then "Send Transaction". You'll see the whole flow happen right in the browser console.

If everything works, you'll see:
```
Connecting to Bifrost at https://127.0.0.1:4433
Connected to Bifrost successfully
Generated keypair: ...
Requesting airdrop of 2 SOL
Airdrop confirmed!
...
Transaction sent successfully
```

Boom. Your transaction just went directly to the validator's TPU.

## What Just Happened?

1. The client connected to Bifrost over WebTransport (like WebSockets but faster)
2. Bifrost received your transaction
3. Bifrost forwarded it to the validator's TPU via QUIC
4. The validator processed it

No RPC in the middle. Just you → Bifrost → validator.

## Browser Support

WebTransport works in:
- Chrome/Edge 97+
- Firefox (behind a flag)
- Safari (not yet)

For Safari users, you'll need to use the Rust client for now, sorry.

## Troubleshooting

**"Airdrop timeout"**
- Make sure `solana-test-validator` is actually running
- Try restarting it with `solana-test-validator --reset`

**"Failed to establish QUIC connection"**
- Check that the validator is running (you should see logs)
- The validator's TPU port is usually 8009 for local testing

**"Certificate error"**
- Regenerate your certs with the command above
- Make sure they're in the `certs/` directory

## Using in Production

For mainnet, you'll need:
- Real TLS certificates (not self-signed)
- Leader schedule tracking (to send to the right validator)
- Proper error handling and retries

But for now, this gets you up and running locally.

## License

---

Questions? Issues? PRs welcome. Keep it chill. ✌️
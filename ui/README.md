# A.R.E.S Chat UI

A modern chat interface for the A.R.E.S (Agentic Reasoning & Execution System) server. The UI uses **Leptos** and **Tailwind CSS**.

## Features

- 🎨 **Modern Dark Theme** - clean, professional design with smooth animations
- 💬 **Real-time Chat** - send messages and receive AI responses
- 🤖 **Agent Selection** - choose a specific agent or use auto-routing
- 🔧 **Tool Call Display** - see when the AI uses tools (calculator, search, and more)
- 📝 **Markdown Support** - code blocks and inline code rendering
- 💾 **Persistent Auth** - JWT-based authentication with localStorage
- 📱 **Responsive Design** - works on desktop and mobile

## Tech Stack

- **[Leptos](https://leptos.dev/)** - reactive web framework in Rust
- **[Tailwind CSS](https://tailwindcss.com/)** - utility-first CSS framework
- **[Trunk](https://trunkrs.dev/)** - WASM web application bundler
- **[gloo](https://gloo-rs.web.app/)** - web API bindings for Rust/WASM

## Prerequisites

1. **Rust** with the `wasm32-unknown-unknown` target:

   ```bash
   rustup target add wasm32-unknown-unknown
   ```

2. **Trunk** bundler:

   ```bash
   cargo install trunk --locked
   ```

3. **Node.js** (for Tailwind CSS):

   ```bash
   npm install
   ```

## Development

### Quick Start

Run these commands from the project root:

```bash
# Install all dependencies
just ui-setup

# Start the dev server (opens browser)
just ui-dev
```

### Manual Commands

```bash
cd ui

# Install npm dependencies
npm install

# Build Tailwind CSS
npm run build:css

# Start development server with hot reload
trunk serve --open

# Build for production
trunk build --release
```

### Running with Backend

Start the ARES server and the UI:

```bash
# Terminal 1: Start backend
just run

# Terminal 2: Start UI
just ui-dev
```

Alternatively, use the combined command:

```bash
just dev
```

- **Backend**: http://localhost:3000
- **UI**: http://localhost:8080

## Project Structure

```
ui/
├── Cargo.toml          # Rust dependencies
├── Trunk.toml          # Trunk bundler config
├── index.html          # HTML entry point
├── input.css           # Tailwind input CSS
├── tailwind.config.js  # Tailwind configuration
├── package.json        # Node dependencies (Tailwind)
└── src/
    ├── main.rs         # Entry point
    ├── lib.rs          # App component & routing
    ├── api.rs          # API client functions
    ├── state.rs        # Global app state
    ├── types.rs        # Type definitions
    ├── components/     # Reusable UI components
    │   ├── mod.rs
    │   ├── chat_input.rs
    │   ├── chat_message.rs
    │   ├── header.rs
    │   ├── loading.rs
    │   └── sidebar.rs
    └── pages/          # Page components
        ├── mod.rs
        ├── home.rs
        ├── login.rs
        └── chat.rs
```

## Configuration

### API Base URL

The UI defaults to `http://localhost:3000`. To change the URL, modify `api_base` in `src/state.rs` or implement environment-based configuration.

### CORS

The ARES backend must allow requests from the UI origin through its `cors_origins` configuration. In development, the UI origin is typically `http://localhost:8080`.

## Production Build

```bash
# Build optimized WASM bundle
cd ui && trunk build --release

# Output is in ui/dist/
```

The `dist/` folder contains static files. Any web server (nginx, Caddy, S3, and more) can serve them.

## Troubleshooting

### Tailwind styles not applying

1. Run `npm install`.
2. Make sure that `dist/output.css` exists.
3. If the file does not exist, run `npm run build:css` manually.

### WASM compilation errors

1. If the `wasm32-unknown-unknown` target is not installed, install it:

   ```bash
   rustup target add wasm32-unknown-unknown
   ```

### Connection refused errors

1. Make sure that the ARES backend runs on port 3000.
2. Check the browser console for CORS errors.

## License

MIT License - see [LICENSE](../LICENSE) in the project root.

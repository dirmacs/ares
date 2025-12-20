# A.R.E.S Chat UI

A modern, sleek chat interface for the A.R.E.S (Agentic Reasoning & Execution System) server, built with **Leptos** and **Tailwind CSS**.

## Features

- 🎨 **Modern Dark Theme** - Clean, professional design with smooth animations
- 💬 **Real-time Chat** - Send messages and receive AI responses
- 🤖 **Agent Selection** - Choose specific agents or use auto-routing
- 🔧 **Tool Call Display** - See when the AI uses tools (calculator, search, etc.)
- 📝 **Markdown Support** - Code blocks and inline code rendering
- 💾 **Persistent Auth** - JWT-based authentication with localStorage
- 📱 **Responsive Design** - Works on desktop and mobile

## Tech Stack

- **[Leptos](https://leptos.dev/)** - Rust-based reactive web framework
- **[Tailwind CSS](https://tailwindcss.com/)** - Utility-first CSS framework
- **[Trunk](https://trunkrs.dev/)** - WASM web application bundler
- **[gloo](https://gloo-rs.web.app/)** - Web API bindings for Rust/WASM

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

From the project root:

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

Start both the ARES server and UI:

```bash
# Terminal 1: Start backend
just run

# Terminal 2: Start UI
just ui-dev
```

Or use the combined command:

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
    │   ├── agent_selector.rs
    │   └── sidebar.rs
    └── pages/          # Page components
        ├── mod.rs
        ├── home.rs
        ├── login.rs
        └── chat.rs
```

## Configuration

### API Base URL

The UI defaults to `http://localhost:3000`. To change it, modify the `api_base` in `src/state.rs` or implement environment-based configuration.

### CORS

Ensure the ARES backend has CORS configured to allow requests from the UI origin (typically `http://localhost:8080` in development).

## Production Build

```bash
# Build optimized WASM bundle
cd ui && trunk build --release

# Output is in ui/dist/
```

The `dist/` folder contains static files that can be served by any web server (nginx, Caddy, S3, etc.).

## Troubleshooting

### Tailwind styles not applying

1. Ensure `npm install` was run
2. Check that `dist/output.css` is generated
3. Run `npm run build:css` manually

### WASM compilation errors

1. Ensure `wasm32-unknown-unknown` target is installed:
   ```bash
   rustup target add wasm32-unknown-unknown
   ```

### Connection refused errors

1. Ensure the ARES backend is running on port 3000
2. Check browser console for CORS errors

## License

MIT License - see [LICENSE](../LICENSE) in the project root.

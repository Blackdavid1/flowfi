# Contributing to FlowFi

Thank you for your interest in contributing to **FlowFi**

FlowFi is a DeFi payment streaming protocol built on Stellar using Soroban smart contracts. This guide explains how to set up your local development environment and contribute effectively.

Please read this document carefully before opening a Pull Request.

## Getting Help & Asking Questions

Have questions before contributing? We've got you covered!

- **Questions about using FlowFi?** → Start a discussion in [GitHub Discussions - Q&A](https://github.com/LabsCrypt/flowfi/discussions/categories/q-a)
- **Found a bug?** → [Open an Issue](https://github.com/LabsCrypt/flowfi/issues)
- **Want to suggest a feature?** → [Start a Discussion - Ideas](https://github.com/LabsCrypt/flowfi/discussions/categories/ideas)
- **Need help setting up?** → Check [Local Development Setup](#local-development-setup) or ask in Discussions

### Issues vs Discussions

**Open an Issue if:**

- You found a bug 🐛
- You want to work on a concrete feature or task ✨
- There's a documentation problem 📝

**Start a Discussion if:**

- You have a question ❓
- You want to propose and discuss a feature 💡
- You're sharing a project or use case 🎪

See our [Discussions Guide](DISCUSSIONS.md) for more details.

---

## Table of Contents

- [Getting Help & Asking Questions](#getting-help--asking-questions)
- [Project Overview](#project-overview)
- [Local Development Setup](#local-development-setup)
- [Accessibility: Motion & Contrast](#accessibility-motion--contrast)
- [Branching Strategy](#branching-strategy)
- [Commit Guidelines & Hooks](#commit-guidelines--hooks)
- [Pull Request Process](#pull-request-process)
- [Security](#security)
- [Code of Conduct](#code-of-conduct)

---

## Project Overview

FlowFi is structured as a monorepo:

```
flowfi/
├── backend/      # Express.js + TypeScript backend
├── contracts/    # Soroban smart contracts (Rust)
├── frontend/     # Next.js + Tailwind CSS frontend
```

Technologies used:

- **Frontend**: Next.js + TypeScript + Tailwind CSS
- **Backend**: Express.js + TypeScript
- **Smart Contracts**: Rust + Soroban
- **Database**: PostgreSQL
- **Containerization**: Docker & Docker Compose

---

# Local Development Setup

## Fork & Clone the Repository

Fork & Clone the Repository

First, fork the repository on GitHub.

Then clone your fork locally:

```bash
git clone https://github.com/YOUR-USERNAME/flowfi.git
cd flowfi
```

## Prerequisites

Make sure you have the following installed:

- Node.js (LTS recommended)
- npm
- Rust & Cargo
- Docker & Docker Compose
- (Optional) Stellar CLI

---

## Option 1: Docker (Recommended)

The fastest way to run the full stack locally:

```bash
docker compose up --build
```

This starts:

- PostgreSQL (port 5433)
- Backend API (port 3001)

To run in detached mode:

```bash
docker compose up -d --build
```

To stop services:

```bash
docker compose down
```

To reset the database:

```bash
docker compose down -v
```

---

## Option 2: Manual Setup

### Backend Setup

1. **Install Dependencies**

```bash
cd backend
npm install
```

2. **Set Up Database**

The backend uses PostgreSQL. You can either:

- Use Docker Compose (recommended): `docker compose up postgres -d`
- Or set up PostgreSQL locally and configure `DATABASE_URL` in your `.env` file

3. **Run Database Migrations**

```bash
npm run prisma:generate
npm run prisma:migrate
```

4. **Start Development Server**

```bash
npm run dev
```

Backend runs on: `http://localhost:3001`

**Available Backend Scripts:**

- `npm run dev` - Start development server with hot reload
- `npm run build` - Build TypeScript to JavaScript
- `npm run start` - Start production server
- `npm run test` - Run test suite
- `npm run prisma:generate` - Generate Prisma client
- `npm run prisma:migrate` - Run database migrations
- `npm run prisma:studio` - Open Prisma Studio (database GUI)

**Backend API Documentation:**

- Swagger UI: `http://localhost:3001/api-docs`
- OpenAPI Spec: `http://localhost:3001/api-docs.json`

---

### Frontend Setup

1. **Install Dependencies**

```bash
cd frontend
npm install
```

2. **Start Development Server**

```bash
npm run dev
```

Frontend runs on: `http://localhost:3000`

**Available Frontend Scripts:**

- `npm run dev` - Start Next.js development server
- `npm run build` - Build for production
- `npm run start` - Start production server
- `npm run lint` - Run ESLint

**Environment Variables:**
Copy [`frontend/.env.example`](../frontend/.env.example) to `frontend/.env.local` and adjust as needed. The `.env.example` file documents every `NEXT_PUBLIC_*` variable the application reads, with sensible testnet defaults.

---

### Smart Contracts Setup

1. **Install Rust Toolchain**

Make sure you have Rust and Cargo installed. If not:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

2. **Install Soroban CLI** (if not already installed)

```bash
cargo install --locked soroban-cli
```

3. **Build Contracts**

```bash
cd contracts
cargo build --target wasm32-unknown-unknown --release
```

The compiled WASM files will be in `target/wasm32-unknown-unknown/release/`.

**Contract Development:**

- Contract source: `contracts/stream_contract/src/lib.rs`
- Tests: `contracts/stream_contract/src/test.rs`
- Build target: `wasm32-unknown-unknown`

---

## Development Scripts & Tools

### Root-Level Scripts

From the repository root:

```bash
# Verify security setup
npm run verify-security
```

### Docker Compose Commands

```bash
# Start all services
docker compose up --build

# Start in detached mode
docker compose up -d --build

# View logs
docker compose logs -f

# Stop services
docker compose down

# Reset database (removes volumes)
docker compose down -v
```

---

## Accessibility: Motion & Contrast

Financial views (live balance updates, charts, and transaction status indicators) must
remain usable for people who disable motion or require stronger visual contrast.

### Motion expectations

- Respect the OS-level `prefers-reduced-motion` setting. When it is enabled, live
  balance changes, chart transitions, and transaction status changes must not animate;
  the updated value/state is applied immediately.
- Motion must never be the *only* carrier of information. Every animated change must
  also be conveyed by text, an icon, or an `aria-live` announcement so the same
  information is available with motion disabled.
- When reduced motion is off, animations are allowed but must stay short and must not
  block interaction.

### Contrast expectations

- Text, status indicators, and chart elements must meet **WCAG 2.1 AA** contrast:
  at least **4.5:1** for normal text and **3:1** for large text, icons, and graphical
  chart elements, in both the default and high-contrast presentations.
- High-contrast mode must not rely on color alone to distinguish statuses; pair color
  with a label or icon.
- Verify contrast with a contrast checker (e.g. browser devtools or the WebAIM
  contrast checker) whenever financial colors or status styles change.

### Automated checks

- Add focused tests for the changed behavior, covering both reduced-motion enabled and
  disabled, high-contrast toggling, and status boundary states (e.g. pending → success
  → failure).
- Run the frontend checks before pushing:

```bash
cd frontend
npm run lint
npm run build
```

### Manual verification notes (critical flows)

Before opening a PR that touches financial views, verify the following manually and
note the results in the PR description:

1. **Keyboard only** — Tab through the balance, chart, and transaction status areas;
   every interactive element is reachable and has a visible focus indicator.
2. **Reduced motion** — Enable "reduce motion" in the OS/browser, then trigger a live
   balance update and a transaction status change; confirm no animation plays and the
   new value/state is still announced.
3. **High contrast** — Toggle high-contrast mode and confirm text, status indicators,
   and chart elements remain distinguishable and meet the contrast ratios above.
4. **Screen reader** — With a screen reader (VoiceOver/NVDA), confirm live balance and
   transaction status changes are announced via `aria-live` regions and that status is
   not conveyed by color alone.

---

## CI/CD Workflows

This repository uses GitHub Actions for continuous integration. Workflows are located in `.github/workflows/`.

### Available Workflows

- **Security Checks** (`.github/workflows/security.yml`)
  - Runs on: push to `main`/`develop`, pull requests, and weekly schedule
  - Performs:
    - Dependency vulnerability scanning (`npm audit`)
    - CodeQL analysis for JavaScript/TypeScript
  - View workflow: [Security Checks](.github/workflows/security.yml)

- **CI** (`.github/workflows/ci.yml`)
  - Runs on: push to `main`/`develop` and pull requests
  - Performs:
    - Frontend: lint and build
    - Backend: prisma generation, build, and tests
    - Backend: migration replay validation (see [Database Migrations & Rollout](#database-migrations--rollout))
    - Soroban Contracts: build (wasm) and tests
  - View workflow: [CI](.github/workflows/ci.yml)

### Running CI Checks Locally

Before pushing, ensure your changes pass all the same checks that run in GitHub Actions.

#### 1. Frontend Checks
```bash
cd frontend
npm run lint    # Runs ESLint
npm run build   # Verifies the build
```

#### 2. Backend Checks
```bash
cd backend
npm run prisma:generate  # Ensure Prisma client is up to date
npm run build           # Verifies TypeScript compilation
npm run test            # Runs backend vitest suite
```
*Note: Backend tests require a running PostgreSQL instance and `DATABASE_URL` environment variable.*

#### 3. Smart Contract Checks
```bash
cd contracts
cargo build --target wasm32-unknown-unknown --release  # Verifies contract build
cargo test                                            # Runs contract tests
```

#### 4. Security Verification
```bash
# From the repository root
npm run verify-security
```

#### 5. Migration Replay Validation
```bash
cd backend
npm run test:migrations   # replays migrations from the previous supported schema
```
*Note: This requires a running PostgreSQL instance and `DATABASE_URL` environment variable.
The same check runs in CI as the `migration-replay` job.*

---

## Database Migrations & Rollout

Production PostgreSQL schema changes must be **deployment-safe**: the API and worker
processes are rolled out independently, so for a window of time the *previous* and
*new* application versions run against the *same* database. Every mi

/* … truncated 8348 chars — edit only what you need near the top … */

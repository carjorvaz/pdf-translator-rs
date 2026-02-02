{
  description = "PDF Translator - Rust port with llama.cpp/DeepSeek API support";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, crane, flake-utils, ... }:
    let
      # Qwen3 models - good multilingual support (100+ languages)
      # - Default (4B): ~2.5GB, good balance of quality and resources
      # - Small (1.7B): ~1.1GB, for systems with limited RAM
      # - Quality (8B): ~5.1GB, higher quality if you have the RAM

      # Helper function to create packages for a given system
      mkPackages = system:
        let
          overlays = [ (import rust-overlay) ];
          pkgs = import nixpkgs {
            inherit system overlays;
          };

          rustToolchain = pkgs.rust-bin.stable.latest.default.override {
            extensions = [ "rust-src" "rust-analyzer" ];
          };

          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

          # Common arguments for crane builds
          commonArgs = {
            pname = "pdf-translator";
            src = pkgs.lib.cleanSource ./.;

            nativeBuildInputs = with pkgs; [
              pkg-config
              clang
            ];

            buildInputs = with pkgs; [
              openssl
              mupdf
              freetype
              harfbuzz
              libjpeg
              openjpeg
              jbig2dec
              gumbo
              mujs
              libwebp
              fontconfig
            ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
              darwin.apple_sdk.frameworks.Security
              darwin.apple_sdk.frameworks.SystemConfiguration
            ];

            # mupdf-rs needs these
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        in
        {
          inherit pkgs craneLib commonArgs cargoArtifacts rustToolchain;

          pdf-translator-core = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
            cargoExtraArgs = "-p pdf-translator-core";
          });

          pdf-translator-cli = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
            cargoExtraArgs = "-p pdf-translator-cli";
          });

          pdf-translator-web = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
            cargoExtraArgs = "-p pdf-translator-web";

            # Copy static files to the output (--no-preserve=mode to allow stripping phase to run sed)
            postInstall = ''
              mkdir -p $out/share/pdf-translator-web
              cp -r --no-preserve=mode ${./crates/pdf-translator-web/static} $out/share/pdf-translator-web/static
            '';
          });
        };
    in
    flake-utils.lib.eachDefaultSystem (system:
      let
        inherit (mkPackages system) pkgs craneLib commonArgs cargoArtifacts rustToolchain
          pdf-translator-core pdf-translator-cli pdf-translator-web;

        # Helper script to serve a model (auto-downloads from HuggingFace)
        serve-model = pkgs.writeShellScriptBin "serve-model" ''
          SIZE="''${1:-default}"
          PORT="''${2:-8080}"

          case "$SIZE" in
            small)
              HF_REPO="unsloth/Qwen3-1.7B-GGUF"
              echo "Using Qwen3-1.7B (~1.1GB) - smaller model, lower quality"
              ;;
            quality)
              HF_REPO="unsloth/Qwen3-8B-GGUF"
              echo "Using Qwen3-8B (~5.1GB) - larger model, better quality"
              ;;
            *)
              HF_REPO="unsloth/Qwen3-4B-Instruct-2507-GGUF"
              echo "Using Qwen3-4B (~2.5GB) - recommended balance"
              ;;
          esac

          echo "Starting llama.cpp server on port $PORT"
          echo "Model will be downloaded automatically on first run."
          exec ${pkgs.llama-cpp}/bin/llama-server \
            -hf "$HF_REPO" \
            --port "$PORT" \
            --ctx-size 4096 \
            --n-gpu-layers 0
        '';

        # Test script for the LLM server
        test-llm = pkgs.writeShellScriptBin "test-llm" ''
          echo "Testing llama.cpp server at localhost:8080..."
          ${pkgs.curl}/bin/curl -s http://localhost:8080/v1/chat/completions \
            -H "Content-Type: application/json" \
            -d '{"model": "default_model", "messages": [{"role": "user", "content": "Translate to Chinese: Hello world"}]}' \
            | ${pkgs.jq}/bin/jq -r '.choices[0].message.content'
        '';

      in
      {
        packages = {
          inherit pdf-translator-core pdf-translator-cli pdf-translator-web;
          inherit serve-model test-llm;
          cli = pdf-translator-cli;
          web = pdf-translator-web;
          default = pdf-translator-cli;
        };

        # Apps for `nix run`
        apps = {
          cli = flake-utils.lib.mkApp { drv = pdf-translator-cli; };
          web = flake-utils.lib.mkApp { drv = pdf-translator-web; };
          serve-model = flake-utils.lib.mkApp { drv = serve-model; };
          test-llm = flake-utils.lib.mkApp { drv = test-llm; };
          default = flake-utils.lib.mkApp { drv = pdf-translator-cli; };
        };

        devShells.default = craneLib.devShell {
          packages = with pkgs; [
            # Rust tools
            rustToolchain
            cargo-watch
            cargo-edit

            # Build dependencies
            pkg-config
            openssl
            clang

            # PDF library (mupdf - same as PyMuPDF uses)
            mupdf
            freetype
            harfbuzz
            libjpeg
            openjpeg
            jbig2dec
            gumbo
            mujs
            libwebp
            fontconfig

            # llama.cpp for local LLM inference
            llama-cpp

            # PDF tools for testing
            poppler-utils

            # Utilities
            jq
            curl
          ];

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          shellHook = ''
            echo ""
            echo "╔══════════════════════════════════════════════════════════════╗"
            echo "║           PDF Translator Development Environment             ║"
            echo "╚══════════════════════════════════════════════════════════════╝"
            echo ""
            echo "Quick start:"
            echo "  nix run .#serve-model             # Qwen3-4B (~2.5GB, recommended)"
            echo "  nix run .#serve-model -- small    # Qwen3-1.7B (~1.1GB, low RAM)"
            echo "  nix run .#serve-model -- quality  # Qwen3-8B (~5.1GB, better quality)"
            echo "  nix run .#web                     # Start web UI on :3000"
            echo ""
            echo "Models are downloaded automatically on first run."
            echo ""
            echo "Development:"
            echo "  cargo build                  # Build all crates"
            echo "  cargo clippy                 # Run lints (strict, see Cargo.toml)"
            echo "  cargo test                   # Run tests"
            echo "  nix flake check              # Run all CI checks"
            echo ""
            echo "Test the LLM server:"
            echo "  nix run .#test-llm"
            echo ""
          '';
        };

        checks = {
          inherit pdf-translator-core pdf-translator-cli pdf-translator-web;

          # Clippy with all workspace lints (configured in Cargo.toml)
          clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets --all-features -- --deny warnings";
          });

          # Rustfmt check
          fmt = craneLib.cargoFmt {
            src = craneLib.cleanCargoSource ./.;
          };

          # Documentation check
          doc = craneLib.cargoDoc (commonArgs // {
            inherit cargoArtifacts;
            RUSTDOCFLAGS = "-D warnings";
          });

          # Run tests
          test = craneLib.cargoTest (commonArgs // {
            inherit cargoArtifacts;
          });
        };
      }
    ) // {
      # NixOS module for running pdf-translator-web as a service
      nixosModules.pdf-translator = { config, lib, pkgs, ... }:
        let
          cfg = config.services.pdf-translator;
        in
        {
          options.services.pdf-translator = {
            enable = lib.mkEnableOption "PDF Translator web service";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.stdenv.hostPlatform.system}.pdf-translator-web;
              defaultText = lib.literalExpression "self.packages.\${pkgs.system}.pdf-translator-web";
              description = "The pdf-translator-web package to use.";
            };

            host = lib.mkOption {
              type = lib.types.str;
              default = "127.0.0.1";
              description = "Host address to bind the web server to.";
            };

            port = lib.mkOption {
              type = lib.types.port;
              default = 3000;
              description = "Port to bind the web server to.";
            };

            openFirewall = lib.mkOption {
              type = lib.types.bool;
              default = false;
              description = "Whether to open the firewall for the web server port.";
            };

            apiBase = lib.mkOption {
              type = lib.types.str;
              default = "http://localhost:8080/v1";
              description = "Base URL for the OpenAI-compatible API (e.g., llama.cpp server).";
            };

            apiKeyFile = lib.mkOption {
              type = lib.types.nullOr lib.types.path;
              default = null;
              description = ''
                Path to a file containing the API key for the LLM service.
                The file should contain only the API key, with no trailing newline.
              '';
            };

            model = lib.mkOption {
              type = lib.types.str;
              default = "default_model";
              description = "Model name to use for the OpenAI-compatible API.";
            };

            staticDir = lib.mkOption {
              type = lib.types.path;
              default = "${cfg.package}/share/pdf-translator-web/static";
              defaultText = lib.literalExpression ''"''${cfg.package}/share/pdf-translator-web/static"'';
              description = ''
                Path to the static files directory.
                Defaults to the package's bundled static files.
              '';
            };

            clearCacheOnStart = lib.mkOption {
              type = lib.types.bool;
              default = false;
              description = "Whether to clear the translation cache when the service starts.";
            };

            extraArgs = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [ ];
              description = "Extra command-line arguments to pass to pdf-translator-web.";
            };
          };

          config = lib.mkIf cfg.enable {
            networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [ cfg.port ];

            systemd.services.pdf-translator =
              let
                baseArgs = [
                  "--host" cfg.host
                  "--port" (toString cfg.port)
                  "--api-base" cfg.apiBase
                  "--model" cfg.model
                  "--static-dir" (toString cfg.staticDir)
                ]
                ++ lib.optionals cfg.clearCacheOnStart [ "--clear-cache" ]
                ++ cfg.extraArgs;

                # Wrapper script to handle API key from credentials
                startScript = pkgs.writeShellScript "pdf-translator-start" ''
                  ${lib.optionalString (cfg.apiKeyFile != null) ''
                    export OPENAI_API_KEY="$(cat "$CREDENTIALS_DIRECTORY/api-key")"
                  ''}
                  exec ${cfg.package}/bin/pdf-translator-web ${lib.escapeShellArgs baseArgs}
                '';
              in
              {
                description = "PDF Translator Web Service";
                wantedBy = [ "multi-user.target" ];
                after = [ "network.target" ];

                # Tell the application where systemd's CacheDirectory is located
                # The Rust code uses $XDG_CACHE_HOME/pdf-translator for its cache
                environment.XDG_CACHE_HOME = "/var/cache";

                serviceConfig = {
                  Type = "simple";
                  DynamicUser = true;
                  StateDirectory = "pdf-translator";
                  CacheDirectory = "pdf-translator";
                  ExecStart = startScript;
                  Restart = "on-failure";
                  RestartSec = "5s";

                  # Security hardening
                  NoNewPrivileges = true;
                  ProtectSystem = "strict";
                  ProtectHome = true;
                  PrivateTmp = true;
                  PrivateDevices = true;
                  ProtectKernelTunables = true;
                  ProtectKernelModules = true;
                  ProtectControlGroups = true;
                  RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
                  RestrictNamespaces = true;
                  RestrictRealtime = true;
                  RestrictSUIDSGID = true;
                  MemoryDenyWriteExecute = true;
                  LockPersonality = true;
                }
                // lib.optionalAttrs (cfg.apiKeyFile != null) {
                  LoadCredential = "api-key:${cfg.apiKeyFile}";
                };
              };
          };
        };

      nixosModules.default = self.nixosModules.pdf-translator;
    };
}

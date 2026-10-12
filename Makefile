ZSH := $(shell command -v zsh 2> /dev/null)

all:

# The daemon behind the asynchronous prompt segments; see p10kd/README.md.
p10kd:
	cargo build --release --manifest-path p10kd/Cargo.toml

p10kd-test:
	cargo test --manifest-path p10kd/Cargo.toml
	$(or $(ZSH),:) -f p10kd/tests/e2e.zsh

.PHONY: all p10kd p10kd-test zwc minify pkg

zwc:
	$(MAKE) -C gitstatus zwc
	$(or $(ZSH),:) -fc 'for f in *.zsh-theme internal/*.zsh; do zcompile -R -- $$f.zwc $$f || exit; done'

minify:
	$(MAKE) -C gitstatus minify
	rm -rf -- .git .gitattributes .gitignore LICENSE Makefile README.md font.md powerlevel10k.png

pkg: zwc
	$(MAKE) -C gitstatus pkg

PREFIX ?= $(HOME)/.local
APP_ID = io.github._0wn3r.Tm02Link

build:
	cargo build --release

install: build
	install -Dm755 target/release/tm02link -t $(PREFIX)/bin
	install -Dm644 data/$(APP_ID).desktop -t $(PREFIX)/share/applications
	install -Dm644 data/$(APP_ID).metainfo.xml -t $(PREFIX)/share/metainfo
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg -t $(PREFIX)/share/icons/hicolor/scalable/apps
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg -t $(PREFIX)/share/icons/hicolor/symbolic/apps

flatpak:
	flatpak-builder --user --install --force-clean build-flatpak build-aux/flatpak/$(APP_ID).json

test:
	cargo test

.PHONY: build install flatpak test

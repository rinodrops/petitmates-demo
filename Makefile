PRODUCT   := PetitMatesDemo
BUNDLE_ID := jp.emotiongraphics.petitmates-demo
VERSION   := 0.1.0
MIN_MACOS := 13.0

BUILD_DIR := build
APP       := $(BUILD_DIR)/$(PRODUCT).app
CONTENTS  := $(APP)/Contents
MACOS_DIR := $(CONTENTS)/MacOS
RES_DIR   := $(CONTENTS)/Resources
EXE       := $(MACOS_DIR)/$(PRODUCT)
ZIP       := $(BUILD_DIR)/$(PRODUCT).zip

APP_ZIP   := $(BUILD_DIR)/$(PRODUCT)-v$(VERSION)-darwin-universal.zip

CHAR_SRC  := assets/bearded_dragon
ICON_SRC  := assets/appicon.png
ICONSET   := $(BUILD_DIR)/AppIcon.iconset
ICNS      := $(RES_DIR)/AppIcon.icns

CERT      := $(APPLE_DEVELOPER_CERTIFICATE_NAME)
TEAM_ID   := $(APPLE_DEVELOPER_TEAM_ID)
APPLE_ID_ := $(APPLE_ID)
APP_PASS  := $(APPLE_DEVELOPER_APP_PASSWORD)

.PHONY: all app zip sign notarize windows-exe clean

all: app

# -----------------------------------------------------------------------
# App bundle
# -----------------------------------------------------------------------

app: $(EXE) $(RES_DIR)/assets/bearded_dragon $(ICNS) $(CONTENTS)/Info.plist

# Universal binary (aarch64 + x86_64)
$(EXE): src/main.rs src/macos.rs src/manifest.rs Cargo.toml | $(MACOS_DIR)
	MACOSX_DEPLOYMENT_TARGET=$(MIN_MACOS) cargo build --release --target aarch64-apple-darwin
	MACOSX_DEPLOYMENT_TARGET=$(MIN_MACOS) cargo build --release --target x86_64-apple-darwin
	lipo -create -output $@ \
		target/aarch64-apple-darwin/release/petitmates-demo \
		target/x86_64-apple-darwin/release/petitmates-demo

$(RES_DIR)/assets/bearded_dragon: | $(RES_DIR)
	mkdir -p $@/sprite
	cp $(CHAR_SRC)/manifest.toml $@/
	cp $(CHAR_SRC)/sprite/*.png  $@/sprite/

$(ICNS): $(ICON_SRC) | $(RES_DIR)
	mkdir -p $(ICONSET)
	sips -z 16    16    $(ICON_SRC) --out $(ICONSET)/icon_16x16.png
	sips -z 32    32    $(ICON_SRC) --out $(ICONSET)/icon_16x16@2x.png
	sips -z 32    32    $(ICON_SRC) --out $(ICONSET)/icon_32x32.png
	sips -z 64    64    $(ICON_SRC) --out $(ICONSET)/icon_32x32@2x.png
	sips -z 128   128   $(ICON_SRC) --out $(ICONSET)/icon_128x128.png
	sips -z 256   256   $(ICON_SRC) --out $(ICONSET)/icon_128x128@2x.png
	sips -z 256   256   $(ICON_SRC) --out $(ICONSET)/icon_256x256.png
	sips -z 512   512   $(ICON_SRC) --out $(ICONSET)/icon_256x256@2x.png
	sips -z 512   512   $(ICON_SRC) --out $(ICONSET)/icon_512x512.png
	sips -z 1024  1024  $(ICON_SRC) --out $(ICONSET)/icon_512x512@2x.png
	iconutil -c icns $(ICONSET) -o $@
	rm -rf $(ICONSET)

$(CONTENTS)/Info.plist: | $(CONTENTS)
	/usr/libexec/PlistBuddy \
		-c "Add :CFBundleName              string $(PRODUCT)" \
		-c "Add :CFBundleIdentifier        string $(BUNDLE_ID)" \
		-c "Add :CFBundleExecutable        string $(PRODUCT)" \
		-c "Add :CFBundleVersion           string $(VERSION)" \
		-c "Add :CFBundleShortVersionString string $(VERSION)" \
		-c "Add :CFBundlePackageType       string APPL" \
		-c "Add :LSMinimumSystemVersion    string $(MIN_MACOS)" \
		-c "Add :NSPrincipalClass          string NSApplication" \
		-c "Add :NSHighResolutionCapable   bool   YES" \
		-c "Add :LSUIElement               bool   YES" \
		-c "Add :CFBundleIconFile          string AppIcon" \
		-c "Add :NSHumanReadableCopyright string Copyright 2026 Rino, eMotionGraphics Inc." \
		"$@"

$(BUILD_DIR):
	mkdir -p $@

$(MACOS_DIR):
	mkdir -p $@

$(RES_DIR):
	mkdir -p $@

$(CONTENTS):
	mkdir -p $@

# -----------------------------------------------------------------------
# macOS distribution zip
# -----------------------------------------------------------------------

zip: app
	ditto -c -k --keepParent $(APP) $(APP_ZIP)
	@echo "macOS package: $(APP_ZIP)"

# -----------------------------------------------------------------------
# Signing & notarization
# -----------------------------------------------------------------------

sign: app
	@test -n "$(CERT)" || (echo "Error: APPLE_DEVELOPER_CERTIFICATE_NAME is not set" && exit 1)
	xattr -cr $(APP)
	codesign --deep --force --options runtime \
		--entitlements entitlements.plist \
		--sign "$(CERT)" \
		$(APP)
	@echo "Signed: $(APP)"

notarize: sign
	@test -n "$(TEAM_ID)"   || (echo "Error: APPLE_DEVELOPER_TEAM_ID is not set"        && exit 1)
	@test -n "$(APPLE_ID_)" || (echo "Error: APPLE_ID is not set"                       && exit 1)
	@test -n "$(APP_PASS)"  || (echo "Error: APPLE_DEVELOPER_APP_PASSWORD is not set"   && exit 1)
	ditto -c -k --keepParent $(APP) $(APP_ZIP)
	xcrun notarytool submit $(APP_ZIP) \
		--apple-id  "$(APPLE_ID_)" \
		--password  "$(APP_PASS)" \
		--team-id   "$(TEAM_ID)" \
		--wait
	xcrun stapler staple $(APP)
	@echo "Notarized and stapled: $(APP)"

# -----------------------------------------------------------------------

clean:
	rm -rf $(BUILD_DIR)

# -----------------------------------------------------------------------
# Windows cross-compile (from macOS with mingw-w64)
# Prerequisite: rustup target add x86_64-pc-windows-gnu
# -----------------------------------------------------------------------

WIN_TARGET := x86_64-pc-windows-gnu
WIN_EXE    := target/$(WIN_TARGET)/release/petitmates-demo.exe
WIN_ZIP    := $(BUILD_DIR)/$(PRODUCT)-v$(VERSION)-windows-x86_64.zip

windows-exe: $(WIN_ZIP)

$(WIN_ZIP): $(WIN_EXE) | $(BUILD_DIR)
	@mkdir -p $(BUILD_DIR)/win-pkg/assets/bearded_dragon/sprite
	cp $(WIN_EXE)                    $(BUILD_DIR)/win-pkg/$(PRODUCT).exe
	cp $(CHAR_SRC)/manifest.toml     $(BUILD_DIR)/win-pkg/assets/bearded_dragon/
	cp $(CHAR_SRC)/sprite/*.png      $(BUILD_DIR)/win-pkg/assets/bearded_dragon/sprite/
	cd $(BUILD_DIR)/win-pkg && zip -r ../$(notdir $(WIN_ZIP)) .
	rm -rf $(BUILD_DIR)/win-pkg
	@echo "Windows package: $(WIN_ZIP)"

$(WIN_EXE): src/main.rs src/windows.rs src/manifest.rs build.rs Cargo.toml
	env -u MACOSX_DEPLOYMENT_TARGET cargo build --release --target $(WIN_TARGET)

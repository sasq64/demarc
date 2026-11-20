# Added to amiberry's libretro/Makefile by scripts/build-amiberry-android.sh.
# The __ANDROID__ code in these two is for amiberry's SDL app.
$(AMIBERRY_DIR)/src/osdep/amiberry.o $(AMIBERRY_DIR)/src/osdep/amiberry_filesys.o: FLAGS += -U__ANDROID__
$(TARGET): $(ANDROID_STUB)
LDFLAGS += -static-libstdc++

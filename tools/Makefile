#             __________               __   ___.
#   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
#   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
#   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
#   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
#                     \/            \/     \/    \/            \/
#
CFLAGS := -Os -Wall -Wextra
LDFLAGS :=

CLEANALL := bmp2rb rdf2binary convbdf codepages convttf

all: rdf2binary convbdf codepages bmp2rb

bmp2rb:	bmp2rb.c
	$(SILENT)$(CC) -DAPPLICATION_NAME=\"$@\" $(CFLAGS) $+ -o $@

rdf2binary:	rdf2binary.c
	$(SILENT)$(CC) $(CFLAGS) $+ -o $@

convbdf: convbdf.c
	$(SILENT)$(CC) $(CFLAGS) $+ -o $@

codepages: codepages.c codepage_tables.c
	$(SILENT)$(CC) $(CFLAGS) $+ -o $@

convttf: convttf.c
	$(call PRINTS,CC $(@F))
	$(SILENT)$(CC) $(CFLAGS) -lm -std=c99 -O2 -Wall -g $+ -o $@ \
		`pkg-config --cflags --libs freetype2`

clean:
	@echo "Cleaning tools"
	$(SILENT)rm -f $(CLEANALL) $(shell for f in $(CLEANALL) ; do \
	    echo $$f.exe $$f.o $$f.obj ; done) *~

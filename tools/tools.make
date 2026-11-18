#             __________               __   ___.
#   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
#   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
#   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
#   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
#                     \/            \/     \/    \/            \/
#

TOOLSCFLAGS := -O -g -W -Wall -Wshadow -pedantic -I$(ROOTDIR)/tools

$(TOOLSDIR)/rdf2binary:	$(TOOLSDIR)/rdf2binary.c
$(TOOLSDIR)/convbdf: $(TOOLSDIR)/convbdf.c
$(TOOLSDIR)/codepages: $(TOOLSDIR)/codepages.c $(TOOLSDIR)/codepage_tables.c

$(TOOLSDIR)/bmp2rb: $(TOOLSDIR)/bmp2rb.c
	$(call PRINTS,CC $(@F))
	$(SILENT)$(HOSTCC) -DAPPLICATION_NAME=\"$@\" $(TOOLSCFLAGS) $+ -o $@

$(TOOLSDIR)/convttf: $(TOOLSDIR)/convttf.c
	$(call PRINTS,CC $(@F))
	$(SILENT)$(HOSTCC) $(TOOLSFLAGS) -lm -O2 -Wall -g $+ -o $@ \
		`pkg-config --cflags --libs freetype2`

# implicit rule for simple tools
$(TOOLSDIR)/%: $(TOOLSDIR)/%.c
	$(call PRINTS,CC $(subst $(ROOTDIR)/,,$@))
	$(SILENT)$(HOSTCC) $(TOOLSCFLAGS) -o $@ $^

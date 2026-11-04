.PHONY: ci-export
ci-export:
	@mkdir -p $(OUTPUTDIR)
	$(file >$(OUTPUTDIR)/ci-sources.txt,$(ALL_SRCS))
	$(file >$(OUTPUTDIR)/ci-cflags.txt,$(CFLAGS))
	$(file >$(OUTPUTDIR)/ci-cc-only-cflags.txt,$(CC_ONLY_CFLAGS))
	$(file >$(OUTPUTDIR)/ci-cxx-only-cflags.txt,$(CXX_ONLY_CFLAGS))
	$(file >$(OUTPUTDIR)/ci-libspdm-sources.txt,$(LIBSPDM_SOURCES))
	$(file >$(OUTPUTDIR)/ci-libspdm-cflags.txt,$(addprefix -I,$(LIBSPDM_INCLUDES)) $(addprefix -D,$(LIBSPDM_DEFINES)))
	@echo Exported $(words $(ALL_SRCS)) sources for $(NV_MODULE_LOGGING_NAME)

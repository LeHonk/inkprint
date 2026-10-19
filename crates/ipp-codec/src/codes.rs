//! Numeric constants from RFC 8010 / RFC 8011 and the PWG registrations.

macro_rules! code_enum {
    ($(#[$meta:meta])* $name:ident: $repr:ty { $($variant:ident = $value:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr($repr)]
        pub enum $name {
            $($variant = $value),+
        }

        impl $name {
            pub fn from_repr(v: $repr) -> Option<Self> {
                match v {
                    $($value => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

code_enum! {
    DelimiterTag: u8 {
        Operation = 0x01,
        Job = 0x02,
        End = 0x03,
        Printer = 0x04,
        Unsupported = 0x05,
    }
}

impl DelimiterTag {
    pub fn from_u8(v: u8) -> Option<Self> {
        Self::from_repr(v)
    }
}

code_enum! {
    ValueTag: u8 {
        Unsupported = 0x10,
        Unknown = 0x12,
        NoValue = 0x13,
        Integer = 0x21,
        Boolean = 0x22,
        Enum = 0x23,
        OctetString = 0x30,
        DateTime = 0x31,
        Resolution = 0x32,
        RangeOfInteger = 0x33,
        BegCollection = 0x34,
        TextWithLanguage = 0x35,
        NameWithLanguage = 0x36,
        EndCollection = 0x37,
        TextWithoutLanguage = 0x41,
        NameWithoutLanguage = 0x42,
        Keyword = 0x44,
        Uri = 0x45,
        UriScheme = 0x46,
        Charset = 0x47,
        NaturalLanguage = 0x48,
        MimeMediaType = 0x49,
        MemberAttrName = 0x4a,
    }
}

code_enum! {
    /// Operations this printer implements (IPP Everywhere required set).
    Operation: u16 {
        PrintJob = 0x0002,
        ValidateJob = 0x0004,
        CreateJob = 0x0005,
        SendDocument = 0x0006,
        CancelJob = 0x0008,
        GetJobAttributes = 0x0009,
        GetJobs = 0x000a,
        GetPrinterAttributes = 0x000b,
        CancelMyJobs = 0x0039,
        CloseJob = 0x003b,
        IdentifyPrinter = 0x003c,
    }
}

impl Operation {
    pub fn from_u16(v: u16) -> Option<Self> {
        Self::from_repr(v)
    }

    pub const ALL: &'static [Operation] = &[
        Self::PrintJob,
        Self::ValidateJob,
        Self::CreateJob,
        Self::SendDocument,
        Self::CancelJob,
        Self::GetJobAttributes,
        Self::GetJobs,
        Self::GetPrinterAttributes,
        Self::CancelMyJobs,
        Self::CloseJob,
        Self::IdentifyPrinter,
    ];
}

code_enum! {
    StatusCode: u16 {
        SuccessfulOk = 0x0000,
        SuccessfulOkIgnoredOrSubstitutedAttributes = 0x0001,
        ClientErrorBadRequest = 0x0400,
        ClientErrorNotPossible = 0x0404,
        ClientErrorNotFound = 0x0406,
        ClientErrorDocumentFormatNotSupported = 0x040a,
        ClientErrorAttributesOrValuesNotSupported = 0x040b,
        ServerErrorInternalError = 0x0500,
        ServerErrorOperationNotSupported = 0x0501,
        ServerErrorVersionNotSupported = 0x0503,
        ServerErrorBusy = 0x0507,
    }
}

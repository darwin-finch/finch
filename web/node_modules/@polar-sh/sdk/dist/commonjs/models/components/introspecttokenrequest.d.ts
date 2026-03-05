import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export declare const IntrospectTokenRequestTokenTypeHint: {
    readonly AccessToken: "access_token";
    readonly RefreshToken: "refresh_token";
};
export type IntrospectTokenRequestTokenTypeHint = ClosedEnum<typeof IntrospectTokenRequestTokenTypeHint>;
export type IntrospectTokenRequest = {
    token: string;
    tokenTypeHint?: IntrospectTokenRequestTokenTypeHint | null | undefined;
    clientId: string;
    clientSecret: string;
};
/** @internal */
export declare const IntrospectTokenRequestTokenTypeHint$inboundSchema: z.ZodNativeEnum<typeof IntrospectTokenRequestTokenTypeHint>;
/** @internal */
export declare const IntrospectTokenRequestTokenTypeHint$outboundSchema: z.ZodNativeEnum<typeof IntrospectTokenRequestTokenTypeHint>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace IntrospectTokenRequestTokenTypeHint$ {
    /** @deprecated use `IntrospectTokenRequestTokenTypeHint$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly AccessToken: "access_token";
        readonly RefreshToken: "refresh_token";
    }>;
    /** @deprecated use `IntrospectTokenRequestTokenTypeHint$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly AccessToken: "access_token";
        readonly RefreshToken: "refresh_token";
    }>;
}
/** @internal */
export declare const IntrospectTokenRequest$inboundSchema: z.ZodType<IntrospectTokenRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type IntrospectTokenRequest$Outbound = {
    token: string;
    token_type_hint?: string | null | undefined;
    client_id: string;
    client_secret: string;
};
/** @internal */
export declare const IntrospectTokenRequest$outboundSchema: z.ZodType<IntrospectTokenRequest$Outbound, z.ZodTypeDef, IntrospectTokenRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace IntrospectTokenRequest$ {
    /** @deprecated use `IntrospectTokenRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<IntrospectTokenRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `IntrospectTokenRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<IntrospectTokenRequest$Outbound, z.ZodTypeDef, IntrospectTokenRequest>;
    /** @deprecated use `IntrospectTokenRequest$Outbound` instead. */
    type Outbound = IntrospectTokenRequest$Outbound;
}
export declare function introspectTokenRequestToJSON(introspectTokenRequest: IntrospectTokenRequest): string;
export declare function introspectTokenRequestFromJSON(jsonString: string): SafeParseResult<IntrospectTokenRequest, SDKValidationError>;
//# sourceMappingURL=introspecttokenrequest.d.ts.map
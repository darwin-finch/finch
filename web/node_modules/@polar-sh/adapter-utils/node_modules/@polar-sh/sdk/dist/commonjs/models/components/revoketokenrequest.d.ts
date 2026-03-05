import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export declare const TokenTypeHint: {
    readonly AccessToken: "access_token";
    readonly RefreshToken: "refresh_token";
};
export type TokenTypeHint = ClosedEnum<typeof TokenTypeHint>;
export type RevokeTokenRequest = {
    token: string;
    tokenTypeHint?: TokenTypeHint | null | undefined;
    clientId: string;
    clientSecret: string;
};
/** @internal */
export declare const TokenTypeHint$inboundSchema: z.ZodNativeEnum<typeof TokenTypeHint>;
/** @internal */
export declare const TokenTypeHint$outboundSchema: z.ZodNativeEnum<typeof TokenTypeHint>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace TokenTypeHint$ {
    /** @deprecated use `TokenTypeHint$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly AccessToken: "access_token";
        readonly RefreshToken: "refresh_token";
    }>;
    /** @deprecated use `TokenTypeHint$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly AccessToken: "access_token";
        readonly RefreshToken: "refresh_token";
    }>;
}
/** @internal */
export declare const RevokeTokenRequest$inboundSchema: z.ZodType<RevokeTokenRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type RevokeTokenRequest$Outbound = {
    token: string;
    token_type_hint?: string | null | undefined;
    client_id: string;
    client_secret: string;
};
/** @internal */
export declare const RevokeTokenRequest$outboundSchema: z.ZodType<RevokeTokenRequest$Outbound, z.ZodTypeDef, RevokeTokenRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace RevokeTokenRequest$ {
    /** @deprecated use `RevokeTokenRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<RevokeTokenRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `RevokeTokenRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<RevokeTokenRequest$Outbound, z.ZodTypeDef, RevokeTokenRequest>;
    /** @deprecated use `RevokeTokenRequest$Outbound` instead. */
    type Outbound = RevokeTokenRequest$Outbound;
}
export declare function revokeTokenRequestToJSON(revokeTokenRequest: RevokeTokenRequest): string;
export declare function revokeTokenRequestFromJSON(jsonString: string): SafeParseResult<RevokeTokenRequest, SDKValidationError>;
//# sourceMappingURL=revoketokenrequest.d.ts.map
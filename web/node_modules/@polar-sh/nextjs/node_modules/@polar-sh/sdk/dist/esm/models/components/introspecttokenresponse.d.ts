import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { SubType } from "./subtype.js";
export declare const TokenType: {
    readonly AccessToken: "access_token";
    readonly RefreshToken: "refresh_token";
};
export type TokenType = ClosedEnum<typeof TokenType>;
export type IntrospectTokenResponse = {
    active: boolean;
    clientId: string;
    tokenType: TokenType;
    scope: string;
    subType: SubType;
    sub: string;
    aud: string;
    iss: string;
    exp: number;
    iat: number;
};
/** @internal */
export declare const TokenType$inboundSchema: z.ZodNativeEnum<typeof TokenType>;
/** @internal */
export declare const TokenType$outboundSchema: z.ZodNativeEnum<typeof TokenType>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace TokenType$ {
    /** @deprecated use `TokenType$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly AccessToken: "access_token";
        readonly RefreshToken: "refresh_token";
    }>;
    /** @deprecated use `TokenType$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly AccessToken: "access_token";
        readonly RefreshToken: "refresh_token";
    }>;
}
/** @internal */
export declare const IntrospectTokenResponse$inboundSchema: z.ZodType<IntrospectTokenResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type IntrospectTokenResponse$Outbound = {
    active: boolean;
    client_id: string;
    token_type: string;
    scope: string;
    sub_type: string;
    sub: string;
    aud: string;
    iss: string;
    exp: number;
    iat: number;
};
/** @internal */
export declare const IntrospectTokenResponse$outboundSchema: z.ZodType<IntrospectTokenResponse$Outbound, z.ZodTypeDef, IntrospectTokenResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace IntrospectTokenResponse$ {
    /** @deprecated use `IntrospectTokenResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<IntrospectTokenResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `IntrospectTokenResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<IntrospectTokenResponse$Outbound, z.ZodTypeDef, IntrospectTokenResponse>;
    /** @deprecated use `IntrospectTokenResponse$Outbound` instead. */
    type Outbound = IntrospectTokenResponse$Outbound;
}
export declare function introspectTokenResponseToJSON(introspectTokenResponse: IntrospectTokenResponse): string;
export declare function introspectTokenResponseFromJSON(jsonString: string): SafeParseResult<IntrospectTokenResponse, SDKValidationError>;
//# sourceMappingURL=introspecttokenresponse.d.ts.map
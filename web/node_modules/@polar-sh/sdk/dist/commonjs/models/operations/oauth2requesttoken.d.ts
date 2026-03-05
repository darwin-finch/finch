import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { AuthorizationCodeTokenRequest, AuthorizationCodeTokenRequest$Outbound } from "../components/authorizationcodetokenrequest.js";
import { RefreshTokenRequest, RefreshTokenRequest$Outbound } from "../components/refreshtokenrequest.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type Oauth2RequestTokenRequestBody = RefreshTokenRequest | AuthorizationCodeTokenRequest;
/** @internal */
export declare const Oauth2RequestTokenRequestBody$inboundSchema: z.ZodType<Oauth2RequestTokenRequestBody, z.ZodTypeDef, unknown>;
/** @internal */
export type Oauth2RequestTokenRequestBody$Outbound = RefreshTokenRequest$Outbound | AuthorizationCodeTokenRequest$Outbound;
/** @internal */
export declare const Oauth2RequestTokenRequestBody$outboundSchema: z.ZodType<Oauth2RequestTokenRequestBody$Outbound, z.ZodTypeDef, Oauth2RequestTokenRequestBody>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace Oauth2RequestTokenRequestBody$ {
    /** @deprecated use `Oauth2RequestTokenRequestBody$inboundSchema` instead. */
    const inboundSchema: z.ZodType<Oauth2RequestTokenRequestBody, z.ZodTypeDef, unknown>;
    /** @deprecated use `Oauth2RequestTokenRequestBody$outboundSchema` instead. */
    const outboundSchema: z.ZodType<Oauth2RequestTokenRequestBody$Outbound, z.ZodTypeDef, Oauth2RequestTokenRequestBody>;
    /** @deprecated use `Oauth2RequestTokenRequestBody$Outbound` instead. */
    type Outbound = Oauth2RequestTokenRequestBody$Outbound;
}
export declare function oauth2RequestTokenRequestBodyToJSON(oauth2RequestTokenRequestBody: Oauth2RequestTokenRequestBody): string;
export declare function oauth2RequestTokenRequestBodyFromJSON(jsonString: string): SafeParseResult<Oauth2RequestTokenRequestBody, SDKValidationError>;
//# sourceMappingURL=oauth2requesttoken.d.ts.map
import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { CheckoutUpdatePublic, CheckoutUpdatePublic$Outbound } from "../components/checkoutupdatepublic.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CheckoutsClientUpdateRequest = {
    /**
     * The checkout session client secret.
     */
    clientSecret: string;
    checkoutUpdatePublic: CheckoutUpdatePublic;
};
/** @internal */
export declare const CheckoutsClientUpdateRequest$inboundSchema: z.ZodType<CheckoutsClientUpdateRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutsClientUpdateRequest$Outbound = {
    client_secret: string;
    CheckoutUpdatePublic: CheckoutUpdatePublic$Outbound;
};
/** @internal */
export declare const CheckoutsClientUpdateRequest$outboundSchema: z.ZodType<CheckoutsClientUpdateRequest$Outbound, z.ZodTypeDef, CheckoutsClientUpdateRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutsClientUpdateRequest$ {
    /** @deprecated use `CheckoutsClientUpdateRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutsClientUpdateRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutsClientUpdateRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutsClientUpdateRequest$Outbound, z.ZodTypeDef, CheckoutsClientUpdateRequest>;
    /** @deprecated use `CheckoutsClientUpdateRequest$Outbound` instead. */
    type Outbound = CheckoutsClientUpdateRequest$Outbound;
}
export declare function checkoutsClientUpdateRequestToJSON(checkoutsClientUpdateRequest: CheckoutsClientUpdateRequest): string;
export declare function checkoutsClientUpdateRequestFromJSON(jsonString: string): SafeParseResult<CheckoutsClientUpdateRequest, SDKValidationError>;
//# sourceMappingURL=checkoutsclientupdate.d.ts.map
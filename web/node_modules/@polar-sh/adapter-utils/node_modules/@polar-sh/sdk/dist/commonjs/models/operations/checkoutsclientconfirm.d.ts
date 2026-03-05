import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { CheckoutConfirmStripe, CheckoutConfirmStripe$Outbound } from "../components/checkoutconfirmstripe.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CheckoutsClientConfirmRequest = {
    /**
     * The checkout session client secret.
     */
    clientSecret: string;
    checkoutConfirmStripe: CheckoutConfirmStripe;
};
/** @internal */
export declare const CheckoutsClientConfirmRequest$inboundSchema: z.ZodType<CheckoutsClientConfirmRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutsClientConfirmRequest$Outbound = {
    client_secret: string;
    CheckoutConfirmStripe: CheckoutConfirmStripe$Outbound;
};
/** @internal */
export declare const CheckoutsClientConfirmRequest$outboundSchema: z.ZodType<CheckoutsClientConfirmRequest$Outbound, z.ZodTypeDef, CheckoutsClientConfirmRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutsClientConfirmRequest$ {
    /** @deprecated use `CheckoutsClientConfirmRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutsClientConfirmRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutsClientConfirmRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutsClientConfirmRequest$Outbound, z.ZodTypeDef, CheckoutsClientConfirmRequest>;
    /** @deprecated use `CheckoutsClientConfirmRequest$Outbound` instead. */
    type Outbound = CheckoutsClientConfirmRequest$Outbound;
}
export declare function checkoutsClientConfirmRequestToJSON(checkoutsClientConfirmRequest: CheckoutsClientConfirmRequest): string;
export declare function checkoutsClientConfirmRequestFromJSON(jsonString: string): SafeParseResult<CheckoutsClientConfirmRequest, SDKValidationError>;
//# sourceMappingURL=checkoutsclientconfirm.d.ts.map
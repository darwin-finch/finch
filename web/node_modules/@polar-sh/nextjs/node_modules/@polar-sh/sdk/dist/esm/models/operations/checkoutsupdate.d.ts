import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { CheckoutUpdate, CheckoutUpdate$Outbound } from "../components/checkoutupdate.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CheckoutsUpdateRequest = {
    /**
     * The checkout session ID.
     */
    id: string;
    checkoutUpdate: CheckoutUpdate;
};
/** @internal */
export declare const CheckoutsUpdateRequest$inboundSchema: z.ZodType<CheckoutsUpdateRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutsUpdateRequest$Outbound = {
    id: string;
    CheckoutUpdate: CheckoutUpdate$Outbound;
};
/** @internal */
export declare const CheckoutsUpdateRequest$outboundSchema: z.ZodType<CheckoutsUpdateRequest$Outbound, z.ZodTypeDef, CheckoutsUpdateRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutsUpdateRequest$ {
    /** @deprecated use `CheckoutsUpdateRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutsUpdateRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutsUpdateRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutsUpdateRequest$Outbound, z.ZodTypeDef, CheckoutsUpdateRequest>;
    /** @deprecated use `CheckoutsUpdateRequest$Outbound` instead. */
    type Outbound = CheckoutsUpdateRequest$Outbound;
}
export declare function checkoutsUpdateRequestToJSON(checkoutsUpdateRequest: CheckoutsUpdateRequest): string;
export declare function checkoutsUpdateRequestFromJSON(jsonString: string): SafeParseResult<CheckoutsUpdateRequest, SDKValidationError>;
//# sourceMappingURL=checkoutsupdate.d.ts.map
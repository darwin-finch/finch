import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CheckoutsClientGetRequest = {
    /**
     * The checkout session client secret.
     */
    clientSecret: string;
};
/** @internal */
export declare const CheckoutsClientGetRequest$inboundSchema: z.ZodType<CheckoutsClientGetRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutsClientGetRequest$Outbound = {
    client_secret: string;
};
/** @internal */
export declare const CheckoutsClientGetRequest$outboundSchema: z.ZodType<CheckoutsClientGetRequest$Outbound, z.ZodTypeDef, CheckoutsClientGetRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutsClientGetRequest$ {
    /** @deprecated use `CheckoutsClientGetRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutsClientGetRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutsClientGetRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutsClientGetRequest$Outbound, z.ZodTypeDef, CheckoutsClientGetRequest>;
    /** @deprecated use `CheckoutsClientGetRequest$Outbound` instead. */
    type Outbound = CheckoutsClientGetRequest$Outbound;
}
export declare function checkoutsClientGetRequestToJSON(checkoutsClientGetRequest: CheckoutsClientGetRequest): string;
export declare function checkoutsClientGetRequestFromJSON(jsonString: string): SafeParseResult<CheckoutsClientGetRequest, SDKValidationError>;
//# sourceMappingURL=checkoutsclientget.d.ts.map
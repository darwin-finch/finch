import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomersGetStateRequest = {
    /**
     * The customer ID.
     */
    id: string;
};
/** @internal */
export declare const CustomersGetStateRequest$inboundSchema: z.ZodType<CustomersGetStateRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomersGetStateRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const CustomersGetStateRequest$outboundSchema: z.ZodType<CustomersGetStateRequest$Outbound, z.ZodTypeDef, CustomersGetStateRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomersGetStateRequest$ {
    /** @deprecated use `CustomersGetStateRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomersGetStateRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomersGetStateRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomersGetStateRequest$Outbound, z.ZodTypeDef, CustomersGetStateRequest>;
    /** @deprecated use `CustomersGetStateRequest$Outbound` instead. */
    type Outbound = CustomersGetStateRequest$Outbound;
}
export declare function customersGetStateRequestToJSON(customersGetStateRequest: CustomersGetStateRequest): string;
export declare function customersGetStateRequestFromJSON(jsonString: string): SafeParseResult<CustomersGetStateRequest, SDKValidationError>;
//# sourceMappingURL=customersgetstate.d.ts.map
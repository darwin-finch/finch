import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomersGetRequest = {
    /**
     * The customer ID.
     */
    id: string;
};
/** @internal */
export declare const CustomersGetRequest$inboundSchema: z.ZodType<CustomersGetRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomersGetRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const CustomersGetRequest$outboundSchema: z.ZodType<CustomersGetRequest$Outbound, z.ZodTypeDef, CustomersGetRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomersGetRequest$ {
    /** @deprecated use `CustomersGetRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomersGetRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomersGetRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomersGetRequest$Outbound, z.ZodTypeDef, CustomersGetRequest>;
    /** @deprecated use `CustomersGetRequest$Outbound` instead. */
    type Outbound = CustomersGetRequest$Outbound;
}
export declare function customersGetRequestToJSON(customersGetRequest: CustomersGetRequest): string;
export declare function customersGetRequestFromJSON(jsonString: string): SafeParseResult<CustomersGetRequest, SDKValidationError>;
//# sourceMappingURL=customersget.d.ts.map